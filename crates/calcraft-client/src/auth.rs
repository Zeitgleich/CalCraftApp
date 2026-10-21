//! HTTP Basic credentials (RFC 7617), meant for app passwords.
//!
//! The password and every buffer derived from it (`user:password` and its
//! Base64 form) live in [`Zeroizing`] containers that are wiped on drop. The
//! buffers are allocated with their final capacity so no stale copies are
//! left behind by reallocation. Copies made by the HTTP stack (for example
//! hyper's header map) are outside our control; they are dropped as soon as
//! the request completes.

use std::fmt;

use zeroize::Zeroizing;

use crate::error::Error;
use crate::transport::Secret;

/// Username and (app) password for HTTP Basic authentication.
#[derive(Clone)]
pub struct Credentials {
    username: String,
    password: Zeroizing<String>,
}

impl Credentials {
    /// Creates Basic credentials. The password is moved into a zeroizing
    /// buffer.
    ///
    /// # Errors
    /// [`Error::InvalidInput`] if the user name is empty or contains `:` or
    /// control characters, or the password contains control characters.
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Result<Self, Error> {
        let username = username.into();
        let password = Zeroizing::new(password.into());
        if username.is_empty() || username.contains(':') || username.chars().any(char::is_control) {
            return Err(Error::InvalidInput(
                "user name must be non-empty without ':' or control characters".into(),
            ));
        }
        if password.chars().any(char::is_control) {
            return Err(Error::InvalidInput(
                "password must not contain control characters".into(),
            ));
        }
        Ok(Self { username, password })
    }

    /// The user name.
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Builds the `Authorization: Basic ...` header value.
    pub(crate) fn authorization(&self) -> Secret {
        let plain_len = self.username.len() + 1 + self.password.len();
        let mut plain = Zeroizing::new(Vec::with_capacity(plain_len));
        plain.extend_from_slice(self.username.as_bytes());
        plain.push(b':');
        plain.extend_from_slice(self.password.as_bytes());
        let mut header = Zeroizing::new(String::with_capacity(6 + plain_len.div_ceil(3) * 4));
        header.push_str("Basic ");
        base64_encode(&plain, &mut header);
        Secret::new(header)
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .finish()
    }
}

/// Standard Base64 with padding; writes into a pre-sized buffer.
fn base64_encode(input: &[u8], out: &mut String) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = u32::from(b0) << 16 | u32::from(b1) << 8 | u32::from(b2);
        let symbol = |shift: u32| char::from(ALPHABET[((n >> shift) & 0x3F) as usize]);
        out.push(symbol(18));
        out.push(symbol(12));
        out.push(if chunk.len() > 1 { symbol(6) } else { '=' });
        out.push(if chunk.len() > 2 { symbol(0) } else { '=' });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7617_example() {
        let credentials = Credentials::basic("Aladdin", "open sesame").unwrap();
        assert_eq!(
            credentials.authorization().expose(),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        let credentials = Credentials::basic("a", "bc").unwrap();
        assert_eq!(credentials.authorization().expose(), "Basic YTpiYw==");
        let credentials = Credentials::basic("ab", "c").unwrap();
        assert_eq!(credentials.authorization().expose(), "Basic YWI6Yw==");
    }

    #[test]
    fn utf8_and_validation() {
        let credentials = Credentials::basic("jürgen", "pässword").unwrap();
        assert_eq!(
            credentials.authorization().expose(),
            "Basic asO8cmdlbjpww6Rzc3dvcmQ="
        );
        assert!(Credentials::basic("a:b", "x").is_err());
        assert!(Credentials::basic("", "x").is_err());
        assert!(Credentials::basic("a", "x\r\ny").is_err());
    }

    #[test]
    fn debug_redacts() {
        let credentials = Credentials::basic("alice", "hunter2 secret").unwrap();
        let debug = format!("{credentials:?} {:?}", credentials.authorization());
        assert!(!debug.contains("hunter2"), "{debug}");
        assert!(!debug.contains("aHVudGVy"), "{debug}");
    }
}
