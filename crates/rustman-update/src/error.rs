/// Why an update couldn't be found, downloaded or installed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum UpdateError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[cfg(not(target_os = "macos"))]
    #[error("{0}")]
    Unsupported(String),
    #[error("release {release} has no '{target}*{extension}' asset")]
    NoAsset {
        release: String,
        target: &'static str,
        extension: &'static str,
    },
    #[error("'{0}' not found in archive")]
    NotInArchive(&'static str),
    #[error("extracted binary is too small")]
    TooSmall,
    #[cfg(target_os = "macos")]
    #[error("the release archive doesn't contain a valid signed Rustman.app")]
    InvalidBundle,
    #[error("extracted binary has unknown header {0:02x?} — refusing to replace")]
    NotABinary([u8; 4]),
    #[error("current exe has no parent dir")]
    NoExeDir,
    #[cfg(target_os = "windows")]
    #[error("{0}")]
    Archive(String),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
    #[error("{version} is not newer than the running {current}; refusing to install it")]
    NotNewer { version: String, current: String },
    #[error("invalid Rustman version '{0}'")]
    InvalidVersion(String),
    #[error("refusing a redirect outside GitHub release storage")]
    UntrustedRedirect,
    #[error("GitHub did not return a valid redirect for the release asset")]
    BadRedirect,
    #[error("release {0} has no SHA256SUMS yet")]
    MissingChecksums(String),
    #[error(
        "no published release is accessible in {0}. For a private repository, set RUSTMAN_GITHUB_TOKEN when launching Rustman"
    )]
    NoRelease(&'static str),
    #[error("the download is larger than {} MB; refusing it", .0 / (1024 * 1024))]
    TooLarge(usize),
    #[error("the release's SHA256SUMS isn't text")]
    BadChecksums,
    #[error("the release's SHA256SUMS has no entry for {0}")]
    NoChecksum(String),
    #[error("{0} doesn't match the release's SHA256SUMS; the download is damaged or was altered")]
    ChecksumMismatch(String),
}
