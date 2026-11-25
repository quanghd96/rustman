use std::collections::BTreeMap;
use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use reqwest::{Client, Method, Response, StatusCode, Url, header};
use sha2::{Digest, Sha256};

use super::Config;

pub(super) const LIMIT: usize = 32 * 1024 * 1024;

pub(super) struct Bucket {
    client: Client,
    url: Url,
    host: String,
    config: Config,
}

pub(super) enum Download {
    Missing,
    Unchanged,
    Object { tag: String, data: Vec<u8> },
}

impl Bucket {
    pub(super) fn open(config: Config) -> Result<Self, String> {
        let (url, host) = location(&config)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(25))
            .build()
            .map_err(|_| "Couldn't initialize the cloud connection".to_owned())?;
        Ok(Self {
            client,
            url,
            host,
            config,
        })
    }

    pub(super) async fn download(&self, tag: Option<&str>) -> Result<Download, String> {
        let condition = tag.map(|tag| ("if-none-match", tag));
        let mut response = self.send(Method::GET, Vec::new(), condition).await?;
        match response.status() {
            StatusCode::NOT_FOUND => return Ok(Download::Missing),
            StatusCode::NOT_MODIFIED if tag.is_some() => return Ok(Download::Unchanged),
            StatusCode::OK => {}
            code => return Err(http_error(code)),
        }
        let tag = etag(&response)?;
        if response.content_length().is_some_and(|n| n > LIMIT as u64) {
            return Err("The cloud workspace exceeds 32 MiB".to_owned());
        }
        let mut data = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "Couldn't download the cloud workspace".to_owned())?
        {
            if chunk.len() > LIMIT - data.len() {
                return Err("The cloud workspace exceeds 32 MiB".to_owned());
            }
            data.extend_from_slice(&chunk);
        }
        Ok(Download::Object { tag, data })
    }

    /// A refused precondition means another device got there first.
    pub(super) async fn upload(
        &self,
        data: Vec<u8>,
        tag: Option<&str>,
    ) -> Result<Option<String>, String> {
        let condition = tag.map_or(("if-none-match", "*"), |tag| ("if-match", tag));
        let response = self.send(Method::PUT, data, Some(condition)).await?;
        match response.status() {
            StatusCode::OK => etag(&response).map(Some),
            StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED => Ok(None),
            code => Err(http_error(code)),
        }
    }

    async fn send(
        &self,
        method: Method,
        body: Vec<u8>,
        condition: Option<(&str, &str)>,
    ) -> Result<Response, String> {
        let time = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let headers = signed_headers(
            &self.config,
            &method,
            self.url.path(),
            &self.host,
            &time,
            &body,
            condition,
        );
        let mut request = self.client.request(method.clone(), self.url.clone());
        for (name, value) in headers {
            request = request.header(name, value);
        }
        if method == Method::PUT {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        request.send().await.map_err(|e| {
            if e.is_timeout() {
                "Cloud connection timed out".to_owned()
            } else {
                "Couldn't reach the cloud endpoint. Check the connection and endpoint.".to_owned()
            }
        })
    }
}

pub(super) fn location(config: &Config) -> Result<(Url, String), String> {
    let endpoint = Url::parse(&config.endpoint).map_err(|_| "Enter a valid endpoint URL")?;
    let host = endpoint.host_str().ok_or("The endpoint needs a hostname")?;
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "[::1]");
    if endpoint.scheme() != "https" && !(loopback && endpoint.scheme() == "http") {
        return Err("Use an HTTPS endpoint (HTTP is supported on localhost)".to_owned());
    }
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
    {
        return Err(
            "Enter the endpoint origin, with the bucket and key in their own fields".to_owned(),
        );
    }
    if config.bucket.is_empty()
        || !config
            .bucket
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        || matches!(config.bucket.as_str(), "." | "..")
    {
        return Err("Enter a valid bucket name".to_owned());
    }
    if config
        .key
        .split('/')
        .any(|segment| matches!(segment, "" | "." | ".."))
    {
        return Err("Enter an object key such as rustman/workspace.json".to_owned());
    }
    if config.region.is_empty()
        || !config
            .region
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("Enter the signing region, such as us-east-1 or auto for R2".to_owned());
    }
    if config.access_key.is_empty()
        || !config
            .access_key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err("Enter a valid access key ID".to_owned());
    }
    if config.secret_key.is_empty() || !config.secret_key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("Enter a valid secret access key".to_owned());
    }
    if !config.session_token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("Enter a valid session token".to_owned());
    }
    let host = match endpoint.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let path = config
        .key
        .split('/')
        .map(rustman_core::url::percent_encode)
        .collect::<Vec<_>>()
        .join("/");
    let url = Url::parse(&format!("{}{}/{path}", endpoint.as_str(), config.bucket))
        .map_err(|_| "Invalid bucket or object key".to_owned())?;
    Ok((url, host))
}

fn signed_headers<'a>(
    config: &'a Config,
    method: &Method,
    path: &str,
    host: &str,
    time: &str,
    body: &[u8],
    condition: Option<(&'a str, &str)>,
) -> BTreeMap<&'a str, String> {
    let hash = digest(body);
    let mut headers = BTreeMap::from([
        ("host", host.to_owned()),
        ("x-amz-content-sha256", hash.clone()),
        ("x-amz-date", time.to_owned()),
    ]);
    if !config.session_token.is_empty() {
        headers.insert("x-amz-security-token", config.session_token.clone());
    }
    if let Some((name, value)) = condition {
        headers.insert(name, value.to_owned());
    }
    let names = headers.keys().copied().collect::<Vec<_>>().join(";");
    let canonical_headers: String = headers
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    let canonical_request = format!("{method}\n{path}\n\n{canonical_headers}\n{names}\n{hash}");
    let day = &time[..8];
    let scope = format!("{day}/{}/s3/aws4_request", config.region);
    let key = mac(
        format!("AWS4{}", config.secret_key).as_bytes(),
        day.as_bytes(),
    );
    let key = mac(&key, config.region.as_bytes());
    let key = mac(&key, b"s3");
    let key = mac(&key, b"aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{time}\n{scope}\n{}",
        digest(canonical_request.as_bytes())
    );
    let signature = hex(&mac(&key, to_sign.as_bytes()));
    headers.insert(
        "authorization",
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}",
            config.access_key
        ),
    );
    headers
}

fn mac(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut hmac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    hmac.update(message);
    hmac.finalize().into_bytes().into()
}

pub(super) fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn etag(response: &Response) -> Result<String, String> {
    response
        .headers()
        .get(header::ETAG)
        .and_then(|tag| tag.to_str().ok())
        .filter(|tag| {
            tag.len() > 2
                && tag.starts_with('"')
                && tag.ends_with('"')
                && !tag[1..tag.len() - 1].contains('"')
        })
        .map(str::to_owned)
        .ok_or_else(|| "The endpoint didn't return a strong ETag for the workspace".to_owned())
}

fn http_error(code: StatusCode) -> String {
    if matches!(code, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        format!(
            "Cloud access denied ({code}). Check the keys, signing region and bucket permissions."
        )
    } else {
        format!("The cloud endpoint returned {code}. Check the endpoint, bucket and object key.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_the_aws_get_object_example() {
        let config = Config {
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            ..Config::default()
        };
        let headers = signed_headers(
            &config,
            &Method::GET,
            "/test.txt",
            "examplebucket.s3.amazonaws.com",
            "20130524T000000Z",
            b"",
            Some(("range", "bytes=0-9")),
        );
        assert_eq!(
            headers["authorization"],
            concat!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, ",
                "SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, ",
                "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
            ),
        );
    }

    #[test]
    fn encodes_the_path_style_key_for_signing() {
        let config = Config {
            endpoint: "https://storage.example.org:8443".into(),
            bucket: "my-bucket".into(),
            key: "rustman/api #1+%.json".into(),
            access_key: "custom-key".into(),
            secret_key: "example-secret".into(),
            ..Config::default()
        };
        let (url, host) = location(&config).unwrap();
        assert_eq!(host, "storage.example.org:8443");
        assert_eq!(url.path(), "/my-bucket/rustman/api%20%231%2B%25.json");
    }
}
