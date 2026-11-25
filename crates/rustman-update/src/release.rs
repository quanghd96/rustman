use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, LOCATION};
use reqwest::redirect::Policy;
use reqwest::{Client, Response, StatusCode, Url};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::archive;
use crate::error::UpdateError;
use crate::{Asset, UpdateInfo};

const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

fn api_url(path: &str) -> String {
    let repo = REPOSITORY
        .strip_prefix("https://github.com/")
        .expect("GitHub repository URL");
    format!("https://api.github.com/repos/{repo}/releases/{path}")
}

/// The archive type the updater installs. The macOS release also uploads a
/// DMG whose name holds the same platform token, so the name alone can't
/// pick the right asset.
#[cfg(not(target_os = "windows"))]
const ASSET_EXTENSION: &str = ".tar.gz";
#[cfg(target_os = "windows")]
const ASSET_EXTENSION: &str = ".zip";

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize, Debug)]
struct GhAsset {
    name: String,
    id: u64,
}

/// The checksum list the release workflow uploads, in `sha256sum` format.
const CHECKSUMS_ASSET: &str = "SHA256SUMS";

/// Where an update may come from: GitHub, and the storage hosts its release
/// downloads redirect to. Always over HTTPS.
const DOWNLOAD_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Larger than any release archive; a download past it is refused.
const MAX_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;
const MAX_CHECKSUMS_BYTES: usize = 64 * 1024;
const MAX_REDIRECTS: usize = 10;
const HEX: &[u8; 16] = b"0123456789abcdef";

/// An API token, sent to the GitHub API only, and only when set here rather
/// than picked up from a CI-style `GITHUB_TOKEN`.
const TOKEN_VAR: &str = "RUSTMAN_GITHUB_TOKEN";

pub(crate) async fn check(current: &str) -> Result<Option<UpdateInfo>, UpdateError> {
    let target = asset_target()?;
    let release = fetch_latest(current).await?;
    update_for_release(&release, current, target)
}

fn update_for_release(
    release: &GhRelease,
    current: &str,
    target: &'static str,
) -> Result<Option<UpdateInfo>, UpdateError> {
    let latest = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    if !version_gt(latest, current)? {
        return Ok(None);
    }
    let archive = pick_asset(&release.assets, latest, target, ASSET_EXTENSION)?;
    let checksums = release
        .assets
        .iter()
        .find(|a| a.name == CHECKSUMS_ASSET)
        .ok_or_else(|| UpdateError::MissingChecksums(latest.to_owned()))?;
    Ok(Some(UpdateInfo {
        version: latest.to_owned(),
        current: current.to_owned(),
        asset: Asset {
            name: archive.name.clone(),
            id: archive.id,
            checksums: checksums.id,
        },
    }))
}

/// Installs exactly the release `update` describes, which [`check`] found
/// and the user accepted, after verifying its version and checksum.
pub(crate) async fn install(update: &UpdateInfo, current: &str) -> Result<String, UpdateError> {
    if !version_gt(&update.version, current)? {
        return Err(UpdateError::NotNewer {
            version: update.version.clone(),
            current: current.to_owned(),
        });
    }
    let api = api_client(current)?;
    let storage = download_client(current)?;
    let asset = &update.asset;
    let checksums = download_asset(&api, &storage, asset.checksums, MAX_CHECKSUMS_BYTES).await?;
    let archive = download_asset(&api, &storage, asset.id, MAX_ARCHIVE_BYTES).await?;
    verify(&archive, &asset.name, &checksums)?;
    tokio::task::spawn_blocking(move || archive::replace_running_binary(&archive)).await??;
    Ok(update.version.clone())
}

/// The asset token for this platform and CPU, or why there is none.
fn asset_target() -> Result<&'static str, UpdateError> {
    #[cfg(target_os = "macos")]
    {
        Ok("macos-universal")
    }
    #[cfg(target_os = "linux")]
    {
        match std::env::consts::ARCH {
            "x86_64" => Ok("linux-x86_64"),
            "aarch64" => Ok("linux-aarch64"),
            arch => Err(UpdateError::Unsupported(format!(
                "no Linux release builds for the '{arch}' architecture — build from source instead"
            ))),
        }
    }
    #[cfg(target_os = "windows")]
    {
        match std::env::consts::ARCH {
            "x86_64" => Ok("windows-x86_64"),
            "aarch64" => Ok("windows-aarch64"),
            arch => Err(UpdateError::Unsupported(format!(
                "no Windows release builds for the '{arch}' architecture — build from source instead"
            ))),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(UpdateError::Unsupported(
            "self-update is not supported on this platform".to_owned(),
        ))
    }
}

/// The client for the GitHub API, the only one that may carry a token.
fn api_client(current: &str) -> Result<Client, UpdateError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    if let Ok(token) = std::env::var(TOKEN_VAR) {
        match HeaderValue::from_str(&format!("Bearer {token}")) {
            Ok(mut value) => {
                value.set_sensitive(true);
                headers.insert(AUTHORIZATION, value);
            }
            Err(_) => eprintln!("rustman: ignoring a {TOKEN_VAR} that isn't a valid header"),
        }
    }
    Ok(Client::builder()
        .user_agent(format!("rustman/{current}"))
        .default_headers(headers)
        .redirect(Policy::none())
        .build()?)
}

/// The client for release downloads: no credentials, and redirects only to
/// the [`DOWNLOAD_HOSTS`].
fn download_client(current: &str) -> Result<Client, UpdateError> {
    let policy = Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else if is_trusted(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("refusing a redirect outside GitHub release storage")
        }
    });
    Ok(Client::builder()
        .user_agent(format!("rustman/{current}"))
        .redirect(policy)
        .build()?)
}

async fn fetch_latest(current: &str) -> Result<GhRelease, UpdateError> {
    let response = api_client(current)?.get(api_url("latest")).send().await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(UpdateError::NoRelease(REPOSITORY));
    }
    Ok(response.error_for_status()?.json::<GhRelease>().await?)
}

/// Use the authenticated asset API for private and public releases. The API
/// can either stream the bytes or send a redirect to credential-free storage.
async fn download_asset(
    api: &Client,
    storage: &Client,
    id: u64,
    limit: usize,
) -> Result<Vec<u8>, UpdateError> {
    let response = api
        .get(api_url(&format!("assets/{id}")))
        .header(ACCEPT, "application/octet-stream")
        .send()
        .await?;
    let response = if response.status().is_redirection() {
        let url = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| Url::parse(value).ok())
            .ok_or(UpdateError::BadRedirect)?;
        if !is_trusted(&url) {
            return Err(UpdateError::UntrustedRedirect);
        }
        storage
            .get(url)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?
    } else {
        response
    };
    read_download(response, limit).await
}

fn is_trusted(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| DOWNLOAD_HOSTS.contains(&host))
}

/// The body at `url`, read as it streams in, and refused once it passes
/// `limit` bytes.
async fn read_download(response: Response, limit: usize) -> Result<Vec<u8>, UpdateError> {
    let mut response = response
        .error_for_status()
        .map_err(reqwest::Error::without_url)?;
    let expected = response
        .content_length()
        .map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX));
    if expected > limit {
        return Err(UpdateError::TooLarge(limit));
    }
    let mut body = Vec::with_capacity(expected);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        if body.len() + chunk.len() > limit {
            return Err(UpdateError::TooLarge(limit));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Checks `archive` against its line in `checksums` (`<sha256>  <name>`, as
/// `sha256sum` writes it).
fn verify(archive: &[u8], name: &str, checksums: &[u8]) -> Result<(), UpdateError> {
    let checksums = std::str::from_utf8(checksums).map_err(|_| UpdateError::BadChecksums)?;
    let expected = checksums
        .lines()
        .find_map(|line| {
            let (hash, file) = line.split_once(char::is_whitespace)?;
            let file = file.trim_start().trim_start_matches('*');
            (file == name).then_some(hash)
        })
        .ok_or_else(|| UpdateError::NoChecksum(name.to_owned()))?;
    let actual: String = Sha256::digest(archive)
        .iter()
        .flat_map(|byte| [HEX[usize::from(byte >> 4)], HEX[usize::from(byte & 0xf)]])
        .map(char::from)
        .collect();
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(UpdateError::ChecksumMismatch(name.to_owned()))
    }
}

/// The asset for `target` with `extension`. Assets with `nokit` in their
/// name are a variant older releases also published; the standard build is
/// the one to install.
fn pick_asset<'a>(
    assets: &'a [GhAsset],
    release: &str,
    target: &'static str,
    extension: &'static str,
) -> Result<&'a GhAsset, UpdateError> {
    assets
        .iter()
        .find(|a| {
            !a.name.contains("nokit") && a.name.contains(target) && a.name.ends_with(extension)
        })
        .ok_or_else(|| UpdateError::NoAsset {
            release: release.to_owned(),
            target,
            extension,
        })
}

/// Compare semantic version precedence, including prereleases and ignoring build metadata.
fn version_gt(a: &str, b: &str) -> Result<bool, UpdateError> {
    let parse = |v: &str| Version::parse(v).map_err(|_| UpdateError::InvalidVersion(v.to_owned()));
    Ok(parse(a)?.cmp_precedence(&parse(b)?).is_gt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(version_gt("0.4.0", "0.3.2").unwrap());
        assert!(version_gt("1.0.0", "0.9.9").unwrap());
        assert!(version_gt("0.3.10", "0.3.2").unwrap());
        assert!(version_gt("0.3.2", "0.3.2-dev.5").unwrap());
        assert!(!version_gt("0.3.2+build.2", "0.3.2+build.1").unwrap());
        assert!(!version_gt("0.3.1", "0.3.2").unwrap());
        assert!(!version_gt("0.3.2-dev.5", "0.3.2").unwrap());
        assert!(version_gt("0.3.2-dev.10", "0.3.2-dev.9").unwrap());
        assert!(version_gt("0.3", "0.3.2").is_err());
    }

    fn asset(name: &str) -> GhAsset {
        GhAsset {
            name: name.to_owned(),
            id: 123,
        }
    }

    #[test]
    fn a_newer_release_needs_platform_asset_and_checksums() {
        let name = format!("rustman-macos-universal{ASSET_EXTENSION}");
        let mut release = GhRelease {
            tag_name: "v0.4.1".into(),
            assets: vec![asset(&name)],
        };
        assert!(matches!(
            update_for_release(&release, "0.4.0", "macos-universal"),
            Err(UpdateError::MissingChecksums(version)) if version == "0.4.1"
        ));
        release.assets.push(GhAsset {
            name: CHECKSUMS_ASSET.into(),
            id: 456,
        });
        let update = update_for_release(&release, "0.4.0", "macos-universal")
            .unwrap()
            .unwrap();
        assert_eq!(update.version, "0.4.1");
        assert_eq!(update.asset.name, name);
        assert_eq!(update.asset.id, 123);
        assert_eq!(update.asset.checksums, 456);
    }

    #[test]
    fn picks_the_tarball_not_the_dmg_when_both_contain_the_target() {
        for assets in [
            vec![
                asset("Rustman-macos-universal.dmg"),
                asset("Rustman-macos-universal.tar.gz"),
            ],
            vec![
                asset("Rustman-macos-universal.tar.gz"),
                asset("Rustman-macos-universal.dmg"),
            ],
        ] {
            let picked = pick_asset(&assets, "1.2.3", "macos-universal", ".tar.gz").unwrap();
            assert_eq!(picked.name, "Rustman-macos-universal.tar.gz");
        }
    }

    #[test]
    fn errors_clearly_when_no_matching_asset_exists() {
        let assets = vec![asset("Rustman-macos-universal.dmg")];
        let err = pick_asset(&assets, "1.2.3", "macos-universal", ".tar.gz")
            .unwrap_err()
            .to_string();
        assert_eq!(err, "release 1.2.3 has no 'macos-universal*.tar.gz' asset");
    }

    #[test]
    fn never_picks_a_nokit_asset() {
        let assets = vec![
            asset("rustman-linux-x86_64-nokit.tar.gz"),
            asset("rustman-linux-x86_64.tar.gz"),
        ];
        let picked = pick_asset(&assets, "1.2.3", "linux-x86_64", ".tar.gz").unwrap();
        assert_eq!(picked.name, "rustman-linux-x86_64.tar.gz");
    }

    const ARCHIVE: &[u8] = b"archive bytes";

    fn sums(lines: &[String]) -> Vec<u8> {
        lines.join("\n").into_bytes()
    }

    #[test]
    fn an_archive_is_checked_against_its_own_line() {
        let actual: String = Sha256::digest(ARCHIVE)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let good = sums(&[
            format!("{} other.zip", "0".repeat(64)),
            format!("{actual}  rustman-linux-x86_64.tar.gz"),
        ]);
        verify(ARCHIVE, "rustman-linux-x86_64.tar.gz", &good).unwrap();
        let binary_mode = sums(&[format!("{} *a.zip", actual.to_uppercase())]);
        verify(ARCHIVE, "a.zip", &binary_mode).unwrap();

        let altered = sums(&[format!("{}  a.zip", "f".repeat(64))]);
        assert!(matches!(
            verify(ARCHIVE, "a.zip", &altered),
            Err(UpdateError::ChecksumMismatch(_))
        ));
        assert!(matches!(
            verify(ARCHIVE, "missing.zip", &good),
            Err(UpdateError::NoChecksum(_))
        ));
    }

    #[test]
    fn only_https_github_downloads_are_trusted() {
        for url in [
            "https://github.com/quanghd96/rustman/releases/download/v1/a.zip",
            "https://objects.githubusercontent.com/x",
            "https://release-assets.githubusercontent.com/x",
        ] {
            assert!(Url::parse(url).is_ok_and(|url| is_trusted(&url)), "{url}");
        }
        for url in [
            "http://github.com/a.zip",
            "https://github.com.evil.test/a.zip",
            "https://evil.test/github.com/a.zip",
            "not a url",
        ] {
            assert!(!Url::parse(url).is_ok_and(|url| is_trusted(&url)), "{url}");
        }
    }

    fn update(version: &str) -> UpdateInfo {
        UpdateInfo {
            version: version.to_owned(),
            current: "1.0.0".to_owned(),
            asset: Asset {
                name: "a.zip".into(),
                id: 123,
                checksums: 456,
            },
        }
    }

    #[tokio::test]
    async fn an_install_that_is_not_newer_is_refused_before_any_download() {
        for version in ["1.0.0", "0.9.9"] {
            let err = install(&update(version), "1.0.0").await.unwrap_err();
            assert!(matches!(err, UpdateError::NotNewer { .. }), "{err}");
        }
    }

    /// Serves one response, whatever is asked.
    fn serve(response: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/a.zip", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 4096];
            assert!(stream.read(&mut buf).unwrap() > 0);
            stream.write_all(&response).unwrap();
        });
        url
    }

    #[tokio::test]
    async fn a_download_past_its_limit_is_refused() {
        let client = Client::new();
        let declared = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_vec());
        assert!(matches!(
            read_download(client.get(declared).send().await.unwrap(), 10).await,
            Err(UpdateError::TooLarge(10))
        ));

        let mut chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        chunked.extend_from_slice(b"8\r\n12345678\r\n8\r\n12345678\r\n0\r\n\r\n");
        assert!(matches!(
            read_download(client.get(serve(chunked)).send().await.unwrap(), 10).await,
            Err(UpdateError::TooLarge(10))
        ));

        let small = serve(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc".to_vec());
        assert_eq!(
            read_download(client.get(small).send().await.unwrap(), 10)
                .await
                .unwrap(),
            b"abc"
        );
    }
}
