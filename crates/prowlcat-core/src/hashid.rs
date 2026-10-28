//! Tell what kind of password hash a string looks like, from its form alone.
//!
//! This identifies, it does not decrypt or crack anything. A bare hex digest says only how many
//! bits it has (32 hex digits are an MD5 or an NTLM hash, and nothing in the string tells which),
//! so the answer is a list of candidates, the likeliest first. Formats with a prefix (`$2y$`,
//! `$6$`, `{SSHA}`…) say what they are and give a single answer.

/// What `s` can be, likeliest first; empty when it does not look like a known hash format.
pub fn identify(s: &str) -> Vec<&'static str> {
    let s = s.trim();
    if s.is_empty() {
        return Vec::new();
    }

    // Formats that announce themselves.
    let prefixed: &[(&str, Option<usize>, &[&str])] = &[
        ("$2a$", Some(60), &["bcrypt"]),
        ("$2b$", Some(60), &["bcrypt"]),
        ("$2x$", Some(60), &["bcrypt"]),
        ("$2y$", Some(60), &["bcrypt"]),
        ("$1$", None, &["md5crypt (Unix)"]),
        ("$apr1$", None, &["Apache apr1 (MD5)"]),
        ("$5$", None, &["sha256crypt (Unix)"]),
        ("$6$", None, &["sha512crypt (Unix)"]),
        ("$y$", None, &["yescrypt"]),
        ("$7$", None, &["scrypt (Unix crypt format)"]),
        ("$argon2id$", None, &["Argon2id"]),
        ("$argon2i$", None, &["Argon2i"]),
        ("$argon2d$", None, &["Argon2d"]),
        ("$P$", Some(34), &["phpass (WordPress, phpBB3)"]),
        ("$H$", Some(34), &["phpass (WordPress, phpBB3)"]),
        ("$S$", Some(55), &["Drupal 7 (SHA-512)"]),
        ("$pbkdf2-sha256$", None, &["PBKDF2-SHA256 (passlib)"]),
        ("$pbkdf2-sha512$", None, &["PBKDF2-SHA512 (passlib)"]),
        ("$pbkdf2$", None, &["PBKDF2-SHA1 (passlib)"]),
        ("pbkdf2_sha256$", None, &["PBKDF2-SHA256 (Django)"]),
        ("pbkdf2_sha1$", None, &["PBKDF2-SHA1 (Django)"]),
        ("sha1$", None, &["salted SHA-1 (Django)"]),
        ("md5$", None, &["salted MD5 (Django)"]),
        ("{SSHA512}", None, &["salted SHA-512 (LDAP)"]),
        ("{SSHA}", None, &["salted SHA-1 (LDAP)"]),
        ("{SHA}", None, &["SHA-1, base64 (LDAP)"]),
        ("{MD5}", None, &["MD5, base64 (LDAP)"]),
        ("{CRYPT}", None, &["crypt (LDAP)"]),
    ];
    for (prefix, len, names) in prefixed {
        if s.starts_with(prefix) && len.is_none_or(|n| s.len() == n) {
            return names.to_vec();
        }
    }

    // MySQL 4.1 and later: `*` and 40 upper-case hex digits.
    if s.len() == 41 && s.starts_with('*') && is_hex(&s[1..]) {
        return vec!["MySQL 4.1+ (SHA-1 of SHA-1)"];
    }

    if is_hex(s) {
        return match s.len() {
            8 => vec!["CRC-32", "Adler-32"],
            16 => vec!["MySQL 3.23 (OLD_PASSWORD)", "half of an MD5", "CRC-64"],
            32 => vec!["MD5", "NTLM", "MD4", "LM", "md5(md5(password))"],
            40 => vec!["SHA-1", "RIPEMD-160"],
            48 => vec!["Tiger-192"],
            56 => vec!["SHA-224", "SHA3-224", "Keccak-224"],
            64 => vec!["SHA-256", "SHA3-256", "Keccak-256", "BLAKE2s-256"],
            96 => vec!["SHA-384", "SHA3-384"],
            128 => vec!["SHA-512", "SHA3-512", "Whirlpool", "BLAKE2b-512"],
            _ => Vec::new(),
        };
    }

    // Base64 digests (always padded, so the length and the padding give them away).
    if is_base64(s) {
        match (s.len(), s.ends_with("==")) {
            (24, true) => return vec!["MD5, base64"],
            (28, false) if s.ends_with('=') => return vec!["SHA-1, base64"],
            (44, false) if s.ends_with('=') => return vec!["SHA-256, base64"],
            (88, true) => return vec!["SHA-512, base64"],
            _ => {}
        }
    }
    Vec::new()
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_base64(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

#[cfg(test)]
mod tests {
    use super::identify;

    #[test]
    fn digests_of_the_word_password() {
        assert_eq!(identify("5f4dcc3b5aa765d61d8327deb882cf99")[0], "MD5");
        assert!(identify("8846f7eaee8fb117ad06bdd830b7586c").contains(&"NTLM"));
        assert_eq!(identify("5baa61e4c9b93f3f0682250b6cf8331b7ee68fd8")[0], "SHA-1");
        assert_eq!(identify("5e884898da28047151d0e56f8dc6292773603d0d6aabbdd62a11ef721d1542d8")[0], "SHA-256");
        let sha512 = "b109f3bbbc244eb82441917ed06d618b9008dd09b3befd1b5e07394c706a8bb980b1d7785e5976ec049b46df5f1326af5a2ea6d103fd07c95385ffab0cacbc86";
        assert_eq!(identify(sha512)[0], "SHA-512");
        assert_eq!(identify("5F4DCC3B5AA765D61D8327DEB882CF99")[0], "MD5", "case does not matter");
    }

    #[test]
    fn formats_that_say_what_they_are() {
        assert_eq!(identify("$2y$10$abcdefghijklmnopqrstuuABCDEFGHIJKLMNOPQRSTUVWXYZ01234"), ["bcrypt"]);
        assert_eq!(identify("$6$saltsalt$hashhashhashhashhash"), ["sha512crypt (Unix)"]);
        assert_eq!(identify("$P$B123456789abcdefghijklmnopqrstu"), ["phpass (WordPress, phpBB3)"]);
        assert_eq!(identify("*2470C0C06DEE42FD1618BB99005ADCA2EC9D1E19"), ["MySQL 4.1+ (SHA-1 of SHA-1)"]);
        assert_eq!(identify("{SSHA}AbCdEf0123456789AbCdEf0123456789AbCd"), ["salted SHA-1 (LDAP)"]);
        assert_eq!(identify("pbkdf2_sha256$260000$salt$hash"), ["PBKDF2-SHA256 (Django)"]);
        assert_eq!(identify("$argon2id$v=19$m=65536,t=3,p=4$c2FsdA$aGFzaA"), ["Argon2id"]);
    }

    #[test]
    fn base64_digests() {
        assert_eq!(identify("X03MO1qnZdYdgyfeuILPmQ=="), ["MD5, base64"]);
        assert_eq!(identify("W6ph5Mm5Pz8GgiULbPgzG37mj9g="), ["SHA-1, base64"]);
    }

    #[test]
    fn what_is_not_a_hash() {
        for s in ["", "hunter2", "correct horse battery staple", "5f4dcc3b5aa765d61d8327deb882cf9", "$2y$10$tooshort", "zzzzzzzz"] {
            assert!(identify(s).is_empty(), "{s:?} → {:?}", identify(s));
        }
    }
}
