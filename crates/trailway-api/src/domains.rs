//! Public host names of services: `<label>.<base>`, with the base defaulting
//! to `<ip-with-dashes>.sslip.io` so no DNS setup is needed.

use std::net::IpAddr;

use uuid::Uuid;

/// Longest DNS label is 63; the part derived from names leaves room for a suffix.
const MAX_NAME_PART: usize = 50;
const SUFFIX_LEN: usize = 6;

/// Where `{ip}` stands in a domain base for the server's dashed public IP.
pub const IP_PLACEHOLDER: &str = "{ip}";
pub const DEFAULT_DOMAIN_BASE: &str = "{ip}.sslip.io";

/// Lowercase letters, digits and single hyphens; never empty.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.truncate(MAX_NAME_PART);
    let out = out.trim_matches('-');
    if out.is_empty() {
        "service".into()
    } else {
        out.into()
    }
}

/// The label for a service: `<service>-<environment>`.
pub fn base_label(service: &str, environment: &str) -> String {
    slug(&format!("{service}-{environment}"))
}

/// Label that no other service on the server uses: the plain label when free,
/// otherwise with the start of the service id appended.
pub fn unique_label(base: String, taken: bool, id: Uuid) -> String {
    if taken {
        format!("{base}-{}", &id.simple().to_string()[..SUFFIX_LEN])
    } else {
        base
    }
}

/// `178.104.208.91` becomes `178-104-208-91` (IPv6 colons become dashes too).
pub fn dashed_ip(ip: &str) -> Option<String> {
    let ip: IpAddr = ip.parse().ok()?;
    Some(ip.to_string().replace(['.', ':'], "-"))
}

/// The full host name, or `None` while the server's IP is unknown or invalid.
pub fn domain(label: &str, base: &str, public_ip: Option<&str>) -> Option<String> {
    let base = if base.contains(IP_PLACEHOLDER) {
        base.replace(IP_PLACEHOLDER, &dashed_ip(public_ip?)?)
    } else {
        base.to_string()
    };
    Some(format!("{label}.{}", base.trim_matches('.')))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_dns_safe() {
        assert_eq!(slug("My App!"), "my-app");
        assert_eq!(slug("--a__b--"), "a-b");
        assert_eq!(slug("åäö"), "service");
        assert_eq!(base_label("hello", "production"), "hello-production");
        assert!(slug(&"x".repeat(200)).len() <= MAX_NAME_PART);
    }

    #[test]
    fn taken_labels_get_the_service_id() {
        let id = Uuid::parse_str("abcdef12-0000-0000-0000-000000000000").unwrap();
        assert_eq!(unique_label("web-prod".into(), false, id), "web-prod");
        assert_eq!(unique_label("web-prod".into(), true, id), "web-prod-abcdef");
    }

    #[test]
    fn builds_sslip_and_custom_domains() {
        let ip = Some("178.104.208.91");
        assert_eq!(
            domain("hello-production", DEFAULT_DOMAIN_BASE, ip).unwrap(),
            "hello-production.178-104-208-91.sslip.io"
        );
        assert_eq!(domain("hello-production", DEFAULT_DOMAIN_BASE, None), None);
        assert_eq!(
            domain("hello-production", DEFAULT_DOMAIN_BASE, Some("nope")),
            None
        );
        assert_eq!(
            domain("web-prod", "apps.example.com", None).unwrap(),
            "web-prod.apps.example.com"
        );
        assert_eq!(dashed_ip("2001:db8::1").unwrap(), "2001-db8--1");
    }
}
