//! `S3`-compatible blob storage adapter with `AWS SigV4` signing.
//!
//! Written against the raw HTTP API with a hand-rolled SigV4 signer: the AWS
//! SDK is far too expensive to compile for this workspace, and the adapter
//! only needs Put/Get/Delete.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use kb_ports::{BlobStore, PortError};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

const EMPTY_PAYLOAD_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// S3 connection and bucket settings.
#[derive(Debug, Clone)]
pub struct S3Settings {
    /// Base URL of the object store, without a trailing `/bucket`.
    pub endpoint: String,
    /// Signing region. `RustFS` and `MinIO` accept `us-east-1`.
    pub region: String,
    /// Bucket every key lives in.
    pub bucket: String,
    /// Empty credentials produce anonymous requests, as in the Bun adapter.
    pub access_key_id: String,
    /// Secret half of the credential pair.
    pub secret_access_key: String,
    /// `MinIO` and `RustFS` need path-style addressing; real `AWS` does not.
    pub force_path_style: bool,
    /// Applied to every request, so a hung endpoint cannot wedge a worker.
    pub request_timeout: Duration,
}

impl Default for S3Settings {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:9000".to_string(),
            region: "us-east-1".to_string(),
            bucket: "rantai-kb".to_string(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            force_path_style: true,
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// S3-compatible blob store.
pub struct S3BlobStore {
    settings: S3Settings,
    client: reqwest::Client,
}

/// A request URL together with the canonical URI that must be signed.
struct Target {
    url: String,
    canonical_uri: String,
    host: String,
}

impl S3BlobStore {
    /// Construct a new `S3` blob store from settings.
    ///
    /// # Errors
    ///
    /// Fails when the HTTP client cannot be built, which in practice means an
    /// invalid timeout.
    pub fn new(settings: S3Settings) -> Result<Self, PortError> {
        let client = reqwest::Client::builder()
            .timeout(settings.request_timeout)
            .build()
            .map_err(|e| PortError::Unavailable(format!("s3 client builder: {e}")))?;
        Ok(Self { settings, client })
    }

    fn base(&self) -> String {
        self.settings.endpoint.trim_end_matches('/').to_string()
    }

    /// Resolve an object key to a request URL.
    ///
    /// The key is percent-encoded per path segment: `/` is a separator and must
    /// stay a separator, everything else in a segment is escaped. The same
    /// encoded form is handed to the signer, because SigV4 signs the canonical
    /// URI, not the raw key.
    fn target(&self, key: &str) -> Target {
        let clean = key.trim_start_matches('/');
        let encoded = encode_key(clean);
        let base = self.base();
        let host = authority(&base);

        if self.settings.force_path_style {
            let canonical_uri = format!("/{}/{}", self.settings.bucket, encoded);
            let url = format!("{base}{canonical_uri}");
            Target {
                url,
                canonical_uri,
                host,
            }
        } else {
            let canonical_uri = format!("/{encoded}");
            let url = format!("{base}{canonical_uri}");
            // Virtual-host style moves the bucket into the authority.
            let vhost = virtual_host(&self.settings.bucket, &base);
            Target {
                url,
                canonical_uri,
                host: vhost,
            }
        }
    }

    /// Attach the SigV4 headers to a request. No credentials means an anonymous
    /// request, which is what the empty-credential dev setup uses.
    fn signed(
        &self,
        method: &str,
        target: &Target,
        content_type: Option<&str>,
        payload_hash: &str,
    ) -> reqwest::header::HeaderMap {
        use reqwest::header::HeaderValue;

        let mut headers = reqwest::header::HeaderMap::new();
        if self.settings.access_key_id.is_empty() {
            return headers;
        }

        let mut signed_headers: Vec<(&str, &str)> = vec![("host", target.host.as_str())];
        if let Some(ct) = content_type.filter(|c| !c.is_empty()) {
            if let Ok(v) = HeaderValue::from_bytes(ct.as_bytes()) {
                headers.insert(reqwest::header::CONTENT_TYPE, v);
                signed_headers.push(("content-type", ct));
            }
        }
        signed_headers.push(("x-amz-content-sha256", payload_hash));

        let (amz_date, auth) = sign_v4(
            method,
            &target.canonical_uri,
            "",
            &signed_headers,
            payload_hash,
            &self.settings.access_key_id,
            &self.settings.secret_access_key,
            &self.settings.region,
            "s3",
            Utc::now(),
        );
        for (name, value) in [
            ("x-amz-date", amz_date.as_str()),
            ("x-amz-content-sha256", payload_hash),
            ("authorization", auth.as_str()),
        ] {
            if let Ok(v) = HeaderValue::from_bytes(value.as_bytes()) {
                headers.insert(name, v);
            }
        }
        headers
    }
}

/// Host and optional port of an endpoint URL, as it appears in the `Host` header.
fn authority(endpoint: &str) -> String {
    reqwest::Url::parse(endpoint)
        .ok()
        .and_then(|u| {
            let host = u.host_str()?.to_string();
            Some(match u.port() {
                Some(p) => format!("{host}:{p}"),
                None => host,
            })
        })
        .unwrap_or_else(|| "localhost".to_string())
}

/// Bucket-prefixed authority for virtual-host style addressing.
fn virtual_host(bucket: &str, endpoint: &str) -> String {
    match reqwest::Url::parse(endpoint) {
        Ok(mut u) => {
            let Some(host) = u.host_str().map(str::to_string) else {
                return authority(endpoint);
            };
            u.set_host(Some(&format!("{bucket}.{host}"))).ok();
            match u.port() {
                Some(p) => format!("{bucket}.{host}:{p}"),
                None => format!("{bucket}.{host}"),
            }
        }
        Err(_) => format!("{bucket}.{}", authority(endpoint)),
    }
}

/// Percent-encode an object key one path segment at a time. Unreserved
/// characters pass through; `/` separates segments and is never encoded.
fn encode_key(key: &str) -> String {
    key.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

const HEX: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
];

fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                out.push('%');
                out.push(HEX[usize::from(byte >> 4)]);
                out.push(HEX[usize::from(byte & 0x0f)]);
            }
        }
    }
    out
}

#[async_trait]
impl BlobStore for S3BlobStore {
    async fn upload(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<u64, PortError> {
        let target = self.target(key);
        let payload_hash = hex::encode(Sha256::digest(&body));
        let len = body.len() as u64;

        let headers = self.signed("PUT", &target, Some(content_type), &payload_hash);
        let res = self
            .client
            .put(&target.url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|e| PortError::Unavailable(format!("s3 upload {key}: {e}")))?;

        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            let text = res.text().await.unwrap_or_default();
            return Err(PortError::Unavailable(format!(
                "s3 upload failed status {status}: {text}"
            )));
        }
        Ok(len)
    }

    async fn download(&self, key: &str) -> Result<Vec<u8>, PortError> {
        let target = self.target(key);
        let headers = self.signed("GET", &target, None, EMPTY_PAYLOAD_HASH);
        let res = self
            .client
            .get(&target.url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| PortError::Unavailable(format!("s3 download {key}: {e}")))?;

        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            let text = res.text().await.unwrap_or_default();
            return Err(PortError::Unavailable(format!(
                "s3 download failed status {status}: {text}"
            )));
        }
        let bytes = res
            .bytes()
            .await
            .map_err(|e| PortError::Unavailable(format!("s3 read bytes: {e}")))?;
        Ok(bytes.to_vec())
    }

    async fn delete(&self, key: &str) -> Result<(), PortError> {
        let target = self.target(key);
        let headers = self.signed("DELETE", &target, None, EMPTY_PAYLOAD_HASH);
        let res = self
            .client
            .delete(&target.url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| PortError::Unavailable(format!("s3 delete {key}: {e}")))?;

        let status = res.status().as_u16();
        // A missing object is already in the state the caller asked for.
        if !(200..300).contains(&status) && status != 404 {
            let text = res.text().await.unwrap_or_default();
            return Err(PortError::Unavailable(format!(
                "s3 delete failed status {status}: {text}"
            )));
        }
        Ok(())
    }
}

/// Build the SigV4 `x-amz-date` value and `Authorization` header value.
///
/// Returns `(amz_date, authorization)`. The two empty-string returns are
/// unreachable in practice: HMAC-SHA256 accepts a key of any length, so
/// `new_from_slice` cannot fail.
#[allow(clippy::too_many_arguments)]
pub fn sign_v4(
    method: &str,
    uri_path: &str,
    query_string: &str,
    headers: &[(&str, &str)],
    payload_hash: &str,
    access_key: &str,
    secret_key: &str,
    region: &str,
    service: &str,
    now: DateTime<Utc>,
) -> (String, String) {
    let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let date_stamp = now.format("%Y%m%d").to_string();

    // Canonical headers are sorted by lowercased name and joined with `\n`.
    let mut sorted: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let signed_headers = sorted
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_headers: String = sorted
        .iter()
        .map(|(k, v)| format!("{k}:{v}\n"))
        .collect::<Vec<_>>()
        .concat();

    let canonical_request = format!(
        "{method}\n{uri_path}\n{query_string}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let Ok(signature) = hmac_sha256_hex(
        string_to_sign.as_bytes(),
        signing_key(secret_key, &date_stamp, region, service),
    ) else {
        return (amz_date, String::new());
    };

    let auth_header = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, \
         SignedHeaders={signed_headers}, Signature={signature}"
    );
    (amz_date, auth_header)
}

/// The date/region/service-scoped signing key: `HMAC(HMAC(HMAC(HMAC(k, date),
/// region), service), "aws4_request")`, where `k = "AWS4" + secret`.
#[must_use]
fn signing_key(secret_key: &str, date_stamp: &str, region: &str, service: &str) -> [u8; 32] {
    let k = format!("AWS4{secret_key}");
    let key = hmac_sha256_raw(k.as_bytes(), date_stamp.as_bytes());
    let key = hmac_sha256_raw(&key, region.as_bytes());
    let key = hmac_sha256_raw(&key, service.as_bytes());
    hmac_sha256_raw(&key, b"aws4_request")
}

fn hmac_sha256_raw(key: &[u8], data: &[u8]) -> [u8; 32] {
    let Ok(mut mac) = HmacSha256::new_from_slice(key) else {
        return [0u8; 32];
    };
    mac.update(data);
    let bytes = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

fn hmac_sha256_hex(data: &[u8], key: [u8; 32]) -> Result<String, PortError> {
    let mut mac = HmacSha256::new_from_slice(&key)
        .map_err(|e| PortError::Unavailable(format!("s3 signer: {e}")))?;
    mac.update(data);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// AWS SigV4 test suite, `get-vanilla`: GET / with only host and
    /// x-amz-date signed. A signer that passes this is canonicalised the way
    /// AWS expects; the rest of the suite is variations on the same shape.
    #[test]
    fn sign_v4_matches_aws_get_vanilla_vector() {
        let now = DateTime::from_timestamp(1_440_938_160, 0).unwrap(); // 2015-08-30T12:36:00Z
        let (_, auth) = sign_v4(
            "GET",
            "/",
            "",
            &[
                ("host", "example.amazonaws.com"),
                ("x-amz-date", "20150830T123600Z"),
            ],
            EMPTY_PAYLOAD_HASH,
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "service",
            now,
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn amz_date_matches_the_input_instant() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let (date, auth) = sign_v4(
            "GET",
            "/rantai-kb/test.txt",
            "",
            &[("host", "localhost:9000")],
            EMPTY_PAYLOAD_HASH,
            "kbadmin",
            "kbsecret123",
            "us-east-1",
            "s3",
            now,
        );
        assert_eq!(date, "20231114T221320Z");
        assert!(auth.contains("Credential=kbadmin/20231114/us-east-1/s3/aws4_request"));
        assert!(auth.contains("SignedHeaders=host"));
    }

    #[test]
    fn keys_are_encoded_per_segment_not_whole() {
        assert_eq!(
            encode_key("documents/t1/doc 1/report+final.pdf"),
            "documents/t1/doc%201/report%2Bfinal.pdf"
        );
        // Unreserved characters stay literal so ordinary keys are untouched.
        assert_eq!(encode_key("a/b-c_d.e~f"), "a/b-c_d.e~f");
    }

    #[test]
    fn path_style_target_puts_the_bucket_in_the_path() {
        let store = S3BlobStore::new(S3Settings {
            endpoint: "http://localhost:9000/".into(),
            ..S3Settings::default()
        })
        .unwrap();
        let t = store.target("documents/t1/doc1/a b.pdf");
        assert_eq!(t.canonical_uri, "/rantai-kb/documents/t1/doc1/a%20b.pdf");
        assert_eq!(
            t.url,
            "http://localhost:9000/rantai-kb/documents/t1/doc1/a%20b.pdf"
        );
        assert_eq!(t.host, "localhost:9000");
    }

    #[test]
    fn virtual_host_style_moves_the_bucket_into_the_host() {
        let store = S3BlobStore::new(S3Settings {
            endpoint: "https://s3.us-east-1.amazonaws.com".into(),
            force_path_style: false,
            ..S3Settings::default()
        })
        .unwrap();
        let t = store.target("documents/t1/doc1/a.pdf");
        assert_eq!(t.canonical_uri, "/documents/t1/doc1/a.pdf");
        assert_eq!(t.host, "rantai-kb.s3.us-east-1.amazonaws.com");
    }

    #[test]
    fn no_credentials_means_anonymous_request() {
        let store = S3BlobStore::new(S3Settings::default()).unwrap();
        let t = store.target("k");
        assert!(store.signed("GET", &t, None, EMPTY_PAYLOAD_HASH).is_empty());
    }
}
