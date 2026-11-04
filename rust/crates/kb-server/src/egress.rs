//! Which outbound hosts can receive document or query content.
//!
//! `GET /health` reports `egress`, the sorted, de-duplicated host names of the
//! configured model endpoints that are **not** on this platform. A caller that
//! must not let content leave the host (MOCO-KF's rule) can check this before
//! uploading or searching, instead of trusting the deployment to be local.
//!
//! Everything here is a pure function over strings: no DNS, no sockets, no
//! environment. "Local" is a property of the address as written, so a hostname
//! that resolves to a private address but is spelled `models.example.com` is
//! still reported — the only honest answer without performing the lookup.
//!
//! Local means:
//! * `localhost`;
//! * IPv4 loopback (`127.0.0.0/8`) and the unspecified address;
//! * RFC1918 private IPv4 (`10/8`, `172.16/12`, `192.168/16`);
//! * IPv4 link-local (`169.254/16`);
//! * IPv6 loopback (`::1`), unique-local (`fc00::/7`), link-local (`fe80::/10`)
//!   and unspecified (`::`);
//! * a single-label hostname with no dot, which is how a docker-compose service
//!   such as `tei-lite` or `qdrant` is addressed.
//!
//! IPv4-mapped IPv6 addresses (`::ffff:127.0.0.1`) are classified as the IPv4
//! address they embed.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The host of an absolute URL, lower-cased and without user info or port.
///
/// `None` when the string has no `scheme://authority` or the authority names no
/// host. Brackets around IPv6 literals are removed, so the returned string
/// parses with [`std::net::Ipv6Addr`].
#[must_use]
pub fn host_of_url(url: &str) -> Option<String> {
    let (_scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    // `user:password@host` — only the part after the last `@` is the host.
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_userinfo, host)| host);
    let host = if let Some(after_open) = host_port.strip_prefix('[') {
        let end = after_open.find(']')?;
        &after_open[..end]
    } else {
        // The first colon starts the port. A bare IPv6 literal must be
        // bracketed in a URL; if it is not, this returns its first group, which
        // is the best that can be done without guessing.
        host_port.split(':').next().unwrap_or(host_port)
    };
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Whether a host (as returned by [`host_of_url`]) is on this platform.
#[must_use]
pub fn is_local_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host == "localhost" {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ip_is_local(ip);
    }
    // No dot: a single-label name. Compose service names (`tei-lite`) and bare
    // machine names reach the local network, not the internet.
    !host.contains('.')
}

fn ip_is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_is_local(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return ipv4_is_local(v4);
            }
            ipv6_is_local(v6)
        }
    }
}

fn ipv4_is_local(ip: Ipv4Addr) -> bool {
    ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
}

fn ipv6_is_local(ip: Ipv6Addr) -> bool {
    ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local() || ip.is_unspecified()
}

/// The sorted, de-duplicated hosts of `urls` that are not local.
///
/// Duplicates collapse on the host, not the URL: one cloud provider named by
/// four roles is one destination, and that is what a caller has to reason
/// about.
#[must_use]
pub fn non_local_hosts<I, S>(urls: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let hosts: BTreeSet<String> = urls
        .into_iter()
        .filter_map(|url| host_of_url(url.as_ref()))
        .filter(|host| !is_local_host(host))
        .collect();
    hosts.into_iter().collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_url_yields_its_host_without_port_path_or_credentials() {
        assert_eq!(
            host_of_url("https://openrouter.ai/api/v1/embeddings").as_deref(),
            Some("openrouter.ai")
        );
        assert_eq!(
            host_of_url("http://127.0.0.1:8081/v1/embeddings").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(
            host_of_url("http://tei-lite:80/v1/embeddings").as_deref(),
            Some("tei-lite")
        );
        assert_eq!(
            host_of_url("https://user:secret@example.com:8443/x?y=1#z").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            host_of_url("http://[::1]:8080/v1").as_deref(),
            Some("::1"),
            "IPv6 brackets are stripped so the host parses as an address"
        );
        assert_eq!(host_of_url("not-a-url"), None);
        assert_eq!(host_of_url("https://"), None);
    }

    #[test]
    fn loopback_is_local() {
        for host in ["localhost", "LOCALHOST", "127.0.0.1", "127.1.2.3", "::1"] {
            assert!(is_local_host(host), "{host} should be local");
        }
    }

    #[test]
    fn rfc1918_and_link_local_ipv4_are_local() {
        for host in [
            "10.0.0.1",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.0.1",
            "192.168.255.255",
            "169.254.10.20",
            "0.0.0.0",
        ] {
            assert!(is_local_host(host), "{host} should be local");
        }
        // Just outside the private ranges: routable, therefore not local.
        for host in ["172.15.0.1", "172.32.0.1", "11.0.0.1", "192.169.0.1"] {
            assert!(!is_local_host(host), "{host} should not be local");
        }
    }

    #[test]
    fn ipv6_unique_local_and_link_local_are_local() {
        for host in ["fc00::1", "fd12:3456:789a::1", "fe80::1", "::"] {
            assert!(is_local_host(host), "{host} should be local");
        }
        assert!(
            !is_local_host("2001:4860:4860::8888"),
            "a global IPv6 address is not local"
        );
        // An IPv4-mapped loopback is still loopback.
        assert!(is_local_host("::ffff:127.0.0.1"));
        assert!(is_local_host("::ffff:10.1.2.3"));
        assert!(!is_local_host("::ffff:8.8.8.8"));
    }

    #[test]
    fn a_single_label_hostname_is_local_and_a_dotted_one_is_not() {
        for host in ["tei-lite", "qdrant", "postgres", "models"] {
            assert!(is_local_host(host), "{host} should be local");
        }
        for host in ["openrouter.ai", "api.mistral.ai", "tei.local"] {
            assert!(!is_local_host(host), "{host} should not be local");
        }
    }

    #[test]
    fn hosts_are_sorted_and_deduplicated_across_roles() {
        let urls = [
            "https://openrouter.ai/api/v1/embeddings",
            "https://openrouter.ai/api/v1/rerank",
            "http://tei-lite:80/v1/embeddings",
            "http://127.0.0.1:8081/v1/embeddings",
            "https://api.mistral.ai/v1/ocr",
            "http://[fd00::2]:9000/v1",
        ];
        assert_eq!(
            non_local_hosts(urls),
            vec!["api.mistral.ai".to_string(), "openrouter.ai".to_string()]
        );
    }
}
