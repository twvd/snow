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
//!   Nothing has to remember the mapping: a mangled name resolves by
//!   mangling the names in its directory until one matches;
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

/// The character a Windows host uses for a Mac `\\`: there it is a path
/// separator, and a guest's name must not walk out of the share with it
#[cfg(windows)]
const HOST_BACKSLASH: char = '\u{2216}';

/// Whether a character can be in a volume name: anything that travels in
/// MacRoman, except the Mac path separator and the characters NBP keeps
/// for itself.
///
/// The volume name is also the server's NBP object name, and
/// Inside AppleTalk, 2nd ed., chapter 7, "Registering a name": special
/// characters (the wildcards = and ≈, and the @ separator) are not
/// allowed in the object field.
pub fn volume_name_char_ok(c: char) -> bool {
    !matches!(c, ':' | '=' | '\u{2248}' | '@') && is_representable(c)
}

/// The maximum length of a Macintosh file name
const MAC_NAME_MAX: usize = 31;

/// Whether a host name is hidden on the volume (a dotfile, a
/// `.AppleDouble` entry or a `._` AppleDouble sidecar)
pub fn is_hidden(host_name: &str) -> bool {
    host_name.starts_with('.')
}

/// Whether a host name is a metadata store: the `.AppleDouble` directory
/// and the `._` sidecars
///
/// The names compare without case: on a case-insensitive host (macOS,
/// Windows) `.appledouble` opens the same directory.
pub fn is_container_name(host_name: &str) -> bool {
    host_name.eq_ignore_ascii_case(".AppleDouble") || host_name.starts_with("._")
}

/// Whether a host name is out of the guest's reach: the metadata stores,
/// and `.Parent`, because a file of that name would share its container
/// (`.AppleDouble/.Parent`) with the folder it is in.
///
/// Always hidden, in every hidden-file mode (netatalk does the same),
/// or the guest could rename or delete the metadata.
pub fn is_meta_name(host_name: &str) -> bool {
    is_container_name(host_name) || host_name.eq_ignore_ascii_case(".Parent")
}

/// The Mac name of a host name, and the original name when it was
/// mangled
pub fn to_mac(host_name: &str) -> (String, Option<String>) {
    // The unmodified input is the mangling original (the name the host
    // actually stores; the bijection below must not shadow it)
    let original = host_name;
    // The `:` → `/` bijection, then NFC: macOS stores decomposed
    // names (combining accents have no MacRoman encoding, but their
    // composed forms do)
    let composed: String = from_host(original).nfc().collect();
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
    // Each representable character is one MacRoman byte. The extension is
    // filtered too, and it gives way when it would not leave the hash room;
    // the hash is what keeps the name unique.
    let ext: String = ext
        .chars()
        .filter(|c| is_representable(*c))
        .take(MAC_NAME_MAX - suffix.len())
        .collect();
    let ext = if ext == "." { String::new() } else { ext };
    let max_stem = MAC_NAME_MAX - suffix.len() - ext.chars().count();
    let stem: String = stem.chars().take(max_stem).collect();
    (format!("{stem}{suffix}{ext}"), Some(original.to_string()))
}

/// The host name for a Mac wire name (already MacRoman-decoded to
/// UTF-8 by the caller): the `/` → `:` bijection
pub fn to_host(mac_name: &str) -> String {
    let host = mac_name.replace('/', HOST_SEP.to_string().as_str());
    #[cfg(windows)]
    let host = host.replace('\\', HOST_BACKSLASH.to_string().as_str());
    host
}

/// Whether a host name is one plain path component.
///
/// No parent or current directory, no separator, and on Windows no drive
/// prefix and no name Windows treats specially. Everything that joins a
/// guest's name to a directory checks this, as a created file's path does
/// not go through the share's containment check.
pub fn is_one_component(host_name: &str) -> bool {
    let mut components = std::path::Path::new(host_name).components();
    matches!(components.next(), Some(std::path::Component::Normal(n)) if n == host_name)
        && components.next().is_none()
        && !(cfg!(windows) && is_windows_special(host_name))
}

/// Whether Windows gives a name another meaning than a file of that name.
/// Microsoft's "Naming Files, Paths, and Namespaces": CON, PRN, AUX, NUL,
/// COM1 to COM9 and LPT1 to LPT9 are devices, with any extension too, and
/// a trailing period or space is removed, so "a." opens "a".
fn is_windows_special(name: &str) -> bool {
    if name.ends_with('.') || name.ends_with(' ') {
        return true;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes().get(3..), Some([b'1'..=b'9'])))
}

/// The Mac name for a host name, without mangling: the `:` → `/` bijection
pub fn from_host(host_name: &str) -> String {
    let mac = host_name.replace(HOST_SEP, "/");
    #[cfg(windows)]
    let mac = mac.replace(HOST_BACKSLASH, "\\");
    mac
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
    fn test_windows_special_names() {
        for name in ["CON", "nul", "aux.txt", "Com1", "lpt9.log", "a.", "a "] {
            assert!(is_windows_special(name), "{name}");
        }
        for name in ["CONSOLE", "com10", "lpt0", "a.txt", ".a", "nullify.txt"] {
            assert!(!is_windows_special(name), "{name}");
        }
    }

    #[test]
    fn test_metadata_names_in_any_case() {
        for name in [
            ".AppleDouble",
            ".appledouble",
            ".APPLEDOUBLE",
            ".parent",
            "._x",
        ] {
            assert!(is_meta_name(name), "{name}");
        }
        assert!(!is_meta_name(".AppleDoubles"));
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
    fn test_mangled_names_are_valid_wire_names() {
        for host in [
            "x.this-is-a-long-extension-of-40-characters-ok",
            "report.\u{65e5}\u{672c}\u{8a9e}",
            "\u{65e5}.\u{672c}",
            "a.b\u{65e5}c",
        ] {
            let (mac, original) = to_mac(host);
            assert_eq!(original.as_deref(), Some(host));
            let (bytes, _, unmappable) = MACINTOSH.encode(&mac);
            assert!(!unmappable, "{host:?} -> {mac:?}");
            assert!(bytes.len() <= MAC_NAME_MAX, "{host:?} -> {mac:?}");
            assert!(mac.contains('#'), "{host:?} -> {mac:?}");
        }
        assert_eq!(to_mac("a.b\u{65e5}c").0.rsplit('.').next(), Some("bc"));
    }

    #[test]
    fn test_one_component() {
        assert!(is_one_component("a"));
        assert!(is_one_component("a.b"));
        assert!(is_one_component("..a"));
        assert!(!is_one_component(".."));
        assert!(!is_one_component("."));
        assert!(!is_one_component(""));
        assert!(!is_one_component("a/b"));
        assert!(!is_one_component("/a"));
        // A guest's `/` never gets here as a separator
        assert!(is_one_component(&to_host("a/b")));
        assert!(is_one_component(&to_host("a\\b")));
    }

    #[test]
    fn test_volume_name_chars() {
        assert!(volume_name_char_ok('S'));
        assert!(volume_name_char_ok('\u{e9}'));
        assert!(!volume_name_char_ok(':'));
        for c in ['=', '\u{2248}', '@'] {
            assert!(!volume_name_char_ok(c));
        }
        assert!(!volume_name_char_ok('\u{65e5}'));
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
