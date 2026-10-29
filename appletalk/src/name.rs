//! Name mapping between host file names and AFP wire names
//!
//! The volume serves host files (UTF-8 names) to Macintosh clients
//! (MacRoman names, at most 31 characters). The mapping is:
//!
//! * a `:` on the host becomes a `/` in the Mac name and back (the
//!   classic AppleShare bijection: `/` cannot exist in a host name and
//!   `:` is the AFP path separator, so each side uses the character
//!   the other cannot; on NTFS, where `:` is an alternate-data-stream
//!   marker, a Windows host uses U+2236 RATIO instead);
//! * host names with characters MacRoman cannot represent (or names
//!   longer than 31 Mac characters) are mangled: the offending
//!   characters are dropped from the stem and `#XXXXXXXX` (the
//!   hexadecimal FNV-1a hash of the original UTF-8 name) is appended,
//!   keeping the extension, e.g. `你好.txt` becomes `#a1b2c3d4.txt`.
//!   The reverse mapping is kept in a per-volume table;
//! * names starting with a dot (dotfiles, `.AppleDouble`, `._`
//!   sidecars) are hidden: they are omitted from enumeration and
//!   cannot be addressed on the wire.

use encoding_rs::MACINTOSH;
use unicode_normalization::UnicodeNormalization;

/// The character the host uses for the Mac path separator `/` (the
/// classic AppleShare bijection uses `:`); NTFS cannot hold `:`
/// without creating an alternate data stream, so Windows hosts use
/// U+2236 RATIO (which round-trips through `to_mac` the same way)
#[cfg(not(windows))]
const HOST_SEP: char = ':';
#[cfg(windows)]
const HOST_SEP: char = '\u{2236}';

/// The maximum length of a Macintosh file name
const MAC_NAME_MAX: usize = 31;

/// Whether a host name is hidden on the volume (a dotfile, a
/// `.AppleDouble` entry or a `._` AppleDouble sidecar)
pub fn is_hidden(host_name: &str) -> bool {
    host_name.starts_with('.')
}

/// Whether a host name is the server's own metadata store: the
/// `.AppleDouble` directory and the `._` sidecars.
///
/// Always hidden, in every hidden-file mode (netatalk does the same),
/// or the guest could rename or delete the metadata.
pub fn is_meta_name(host_name: &str) -> bool {
    host_name == ".AppleDouble" || host_name.starts_with("._")
}

/// The Mac name of a host name, and the original name when it was
/// mangled (the caller records the mangled → original pair in the
/// per-volume table)
pub fn to_mac(host_name: &str) -> (String, Option<String>) {
    // The unmodified input is the mangling original (the name the host
    // actually stores; the bijection below must not shadow it)
    let original = host_name;
    // The `:` → `/` bijection, then NFC: macOS stores decomposed
    // names (combining accents have no MacRoman encoding, but their
    // composed forms do)
    let composed: String = original.replace(HOST_SEP, "/").nfc().collect();
    let (encoded, _, unrepresentable) = MACINTOSH.encode(&composed);

    let overlong = encoded.len() > MAC_NAME_MAX;
    if !unrepresentable && !overlong {
        // Canonicalize through MacRoman so that the returned UTF-8
        // encodes back to exactly these MacRoman bytes
        let (decoded, _, _) = MACINTOSH.decode(&encoded);
        return (decoded.into_owned(), None);
    }

    // Mangle: drop the unrepresentable characters from the stem, keep
    // the extension, and append the hash of the original name
    let (stem, ext) = match composed.rfind('.') {
        Some(dot) if dot > 0 => (&composed[..dot], &composed[dot..]),
        _ => (composed.as_str(), ""),
    };
    let stem: String = stem.chars().filter(|c| is_representable(*c)).collect();
    let hash = fnv1a(original.as_bytes());
    let suffix = format!("#{hash:08x}");
    // Keep the mangled name within the Mac limit
    let max_stem = MAC_NAME_MAX.saturating_sub(suffix.len() + ext.len());
    let stem: String = stem.chars().take(max_stem).collect();
    (format!("{stem}{suffix}{ext}"), Some(original.to_string()))
}

/// The host name for a Mac wire name (already MacRoman-decoded to
/// UTF-8 by the caller): the `/` → `:` bijection
pub fn to_host(mac_name: &str) -> String {
    mac_name.replace('/', HOST_SEP.to_string().as_str())
}

/// Whether a character can be represented in MacRoman
fn is_representable(c: char) -> bool {
    !MACINTOSH.encode(&c.to_string()).2 // had_errors
}

/// The 32-bit FNV-1a hash
fn fnv1a(data: &[u8]) -> u32 {
    let mut hash = 0x811c_9dc5u32;
    for b in data {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plain_names_pass_through() {
        let (name, original) = to_mac("hello.txt");
        assert_eq!(name, "hello.txt");
        assert_eq!(original, None);
    }

    #[test]
    fn test_colon_bijection() {
        let (name, original) = to_mac("a:b.txt");
        assert_eq!(name, "a/b.txt");
        assert_eq!(original, None);
        assert_eq!(to_host("a/b.txt"), "a:b.txt");
    }

    #[test]
    fn test_macroman_representable_names_are_not_mangled() {
        // é is in MacRoman
        let (name, original) = to_mac("résumé.txt");
        assert_eq!(name, "résumé.txt");
        assert_eq!(original, None);
    }

    #[test]
    fn test_unrepresentable_names_are_mangled_with_extension() {
        let (name, original) = to_mac("你好.txt");
        assert!(name.starts_with("#"), "got {name}");
        assert!(name.ends_with(".txt"), "got {name}");
        assert_eq!(name.len(), 1 + 8 + 4); // hash + extension
        assert_eq!(original.as_deref(), Some("你好.txt"));
        // Stable across calls
        assert_eq!(to_mac("你好.txt").0, name);
        // Different names mangle differently
        assert_ne!(to_mac("悪い.txt").0, name);
    }

    #[test]
    fn test_mangled_name_keeps_representable_stem() {
        let (name, original) = to_mac("rep\u{4e2d}ort.txt");
        assert!(name.starts_with("report#"), "got {name}");
        assert!(name.ends_with(".txt"));
        assert_eq!(original.as_deref(), Some("rep\u{4e2d}ort.txt"));
    }

    #[test]
    fn test_overlong_names_are_mangled() {
        let long = "a".repeat(40) + ".txt";
        let (name, original) = to_mac(&long);
        assert!(name.ends_with(".txt"));
        assert!(name.len() <= 31, "got {}", name.len());
        assert_eq!(original.as_deref(), Some(long.as_str()));
    }

    #[test]
    fn test_mangled_stem_is_truncated_to_the_limit() {
        // A 40-character unrepresentable stem without an extension
        let long = "漢".repeat(40);
        let (name, original) = to_mac(&long);
        assert_eq!(name.len(), 9); // only the hash, no stem
        assert_eq!(original.as_deref(), Some(long.as_str()));
    }

    #[test]
    fn test_hidden() {
        assert!(is_hidden(".secret"));
        assert!(is_hidden("._sidecar"));
        assert!(is_hidden(".AppleDouble"));
        assert!(!is_hidden("visible.txt"));
        assert!(!is_hidden("#a1b2c3d4.txt"));
    }

    #[test]
    fn test_mangled_original_keeps_colon() {
        // The bijection must not shadow the mangling original: the
        // table entry must be the name the host actually stores
        let (mac, original) = to_mac("a:你.txt");
        assert!(mac.starts_with("a/#"), "got {mac}");
        assert!(mac.ends_with(".txt"), "got {mac}");
        assert_eq!(original.as_deref(), Some("a:你.txt"));
    }

    #[test]
    fn test_nfd_name_not_mangled() {
        // macOS stores résumé.txt decomposed: the combining accents
        // have no MacRoman encoding, but the NFC form does
        let (mac, original) = to_mac("re\u{301}sume\u{301}.txt");
        assert_eq!(mac, "résumé.txt");
        assert_eq!(original, None);
    }

    #[test]
    fn test_nfd_plus_unrepresentable_keeps_the_raw_original() {
        let (mac, original) = to_mac("re\u{301}你.txt");
        assert!(mac.starts_with("ré#"), "got {mac}");
        assert!(mac.ends_with(".txt"), "got {mac}");
        assert_eq!(original.as_deref(), Some("re\u{301}你.txt"));
    }

    #[test]
    fn test_round_trip_of_plain_names() {
        for name in ["hello.txt", "résumé.txt", "a:b.txt"] {
            let (mac, _) = to_mac(name);
            assert_eq!(to_host(&mac), name, "round trip of {name}");
        }
    }
}
