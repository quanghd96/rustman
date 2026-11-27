use std::time::Instant;

use rustman_core::domain::resolved::ResolvedRequest;
use rustman_core::domain::response::Redirect;
use rustman_core::run::{HttpTransport, Response, SendOptions, TransportError};

use crate::client::HttpClient;
use crate::error::SendError;
use crate::request::Built;
use crate::{redirect, request, response};

/// [`HttpTransport`] over a reqwest [`Client`], whose cookie store, pool and
/// TLS settings every request shares.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: HttpClient,
}

impl ReqwestTransport {
    pub fn new(client: impl Into<HttpClient>) -> Self {
        Self {
            client: client.into(),
        }
    }
}

impl HttpTransport for ReqwestTransport {
    async fn send(
        &self,
        request: ResolvedRequest<'_>,
        options: &SendOptions,
    ) -> Result<Response, TransportError> {
        exchange(&self.client, request, options)
            .await
            .map_err(|e| TransportError::new(e.to_string()))
    }
}

/// Sends `request`, following redirects by hand (see [`redirect::follow`]),
/// all within one timeout.
async fn exchange(
    client: &HttpClient,
    request: ResolvedRequest<'_>,
    options: &SendOptions,
) -> Result<Response, SendError> {
    let deadline = Instant::now() + options.timeout;
    let Built {
        request: mut next,
        has_payload,
    } = request::build(&client.client, request)?;
    let mut redirects = Vec::new();
    loop {
        // A streamed body (a multipart upload) can't be sent twice, so its
        // redirect is returned rather than followed.
        let again = next.try_clone();
        *next.timeout_mut() = Some(deadline.saturating_duration_since(Instant::now()));
        let response = client
            .client
            .execute(next)
            .await
            .map_err(SendError::transport)?;
        let followed = again.zip(redirect::target(&response));
        let Some((again, to)) = followed else {
            let mut out = response::read(response, options.body_limit).await?;
            if let Some(tls) = out
                .connection
                .as_mut()
                .and_then(|connection| connection.tls.as_mut())
            {
                tls.verified = client.verifies_certificates;
            }
            out.redirects = redirects;
            return Ok(out);
        };
        if redirects.len() == redirect::MAX_REDIRECTS {
            return Err(SendError::TooManyRedirects);
        }
        redirects.push(Redirect {
            status: response.status().as_u16(),
            to: to.to_string(),
        });
        next = redirect::follow(again, response.status(), to, has_payload)?;
    }
}
