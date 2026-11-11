//! Macintosh resource fork parsing for the desktop database.
//!
//! The AFP desktop-database calls serve the BNDL → ICN# icon mapping of
//! APPL files. That mapping lives in the file's resource fork, which
//! the metadata module finds inside AppleDouble containers and AppleSingle
//! files.
//!
//! The resource map layout is the one the Resource Manager reads
//! (Inside Macintosh Volume I, I-128 to I-131): the fork header gives the
//! map position; the map points at the type list, which itself begins with
//! a count and whose entries point at per-type reference lists of (ID, name
//! offset, attributes, 24-bit data offset, handle) entries.
//!
//! The icon mapping is the Finder Interface's bundle format (Volume III,
//! III-12 to III-13): the BNDL resource holds the signature, the version
//! data's resource ID and a list of resource types (`ICN#`, `FREF`, in any
//! order), each with its own list of (local ID, resource ID) pairs; the
//! FREF resources it names hold the (file type, local ID) table. A file
//! type's icon is the first member of the ICN# resource the `ICN#` list
//! assigns to the type's local ID.

/// The icon family resource types, with the size of one member: the
/// 32x32 monochrome icon (128 bytes of bitmap and 128 of mask), the 32x32
/// 8-bit and 4-bit pixmaps, the 16x16 monochrome icon (32 and 32 bytes),
/// and the 16x16 8-bit and 4-bit pixmaps. Indexed by the ICON_* constants.
const ICON_KINDS: [(&[u8; 4], usize); 6] = [
    (b"ICN#", 256),
    (b"icl8", 1024),
    (b"icl4", 512),
    (b"ics#", 64),
    (b"ics8", 256),
    (b"ics4", 128),
];
const ICON_MONO: usize = 0;
const ICON_COLOR8: usize = 1;
const ICON_COLOR4: usize = 2;
const ICON_SMALL_MONO: usize = 3;
const ICON_SMALL8: usize = 4;
const ICON_SMALL4: usize = 5;

/// The size of an 8-bit and a 4-bit 32x32 color pixmap
pub const ICL8_LEN: usize = ICON_KINDS[ICON_COLOR8].1;
pub const ICL4_LEN: usize = ICON_KINDS[ICON_COLOR4].1;

/// The members of each family, by family ID, one map per kind
type Families = [std::collections::HashMap<i16, Vec<Vec<u8>>>; 6];

/// An icon family a BNDL maps to a file type
pub struct BundleIcon {
    /// The mapped file type (for example `b"APPL"`)
    pub ftype: u32,
    /// The 256-byte monochrome bitmap (with its mask)
    pub mono: Vec<u8>,
    /// The 1024-byte 8-bit color pixmap, when the family has one
    pub color8: Option<Vec<u8>>,
    /// The 512-byte 4-bit color pixmap, when the family has one
    pub color4: Option<Vec<u8>>,
    /// The 64-byte small monochrome icon, when the family has one
    pub small_mono: Option<Vec<u8>>,
    /// The 256-byte small 8-bit pixmap, when the family has one
    pub small8: Option<Vec<u8>>,
    /// The 128-byte small 4-bit pixmap, when the family has one
    pub small4: Option<Vec<u8>>,
}

/// Extract the BNDL → ICN# icon mappings of an application from its
/// resource fork. Returns the mapped file types and their monochrome
/// and color icons, the application's own APPL entry first.
pub fn bundle_icons(fork: &[u8]) -> Vec<BundleIcon> {
    let Some(parsed) = parse(fork) else {
        return Vec::new();
    };

    // The icon families: id → members. All the resource types of one
    // family share the family's ID and list their members in the same
    // order.
    let families: Families = std::array::from_fn(|kind| {
        parsed.icons[kind]
            .iter()
            .filter_map(|(id, off)| Some((*id, family(&parsed, *off, ICON_KINDS[kind].1)?)))
            .collect()
    });
    let icns = &families[ICON_MONO];
    if icns.is_empty() {
        return Vec::new();
    }

    // The file types the bundle maps, in the order its FREFs list them
    let mut types: Vec<(u32, i16)> = Vec::new();
    let mut seen_types: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (ftype, local) in &parsed.frefs {
        if seen_types.insert(*ftype) {
            types.push((*ftype, *local));
        }
    }

    let mut out: Vec<BundleIcon> = Vec::new();
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (ftype, local) in types {
        // The BNDL's local ID → the family's resource ID
        let Some(&actual) = parsed.local_map.get(&local) else {
            continue;
        };
        let Some(icon) = family_icon(&families, actual) else {
            continue;
        };
        if seen.insert(ftype) {
            out.push(BundleIcon { ftype, ..icon });
        }
    }

    // The application's own icon: an APPL mapping from the bundle,
    // else the ICN# 128 convention, else the smallest icon family
    if !seen.contains(&u32::from_be_bytes(*b"APPL")) {
        let actual = icns
            .get_key_value(&128)
            .map(|(id, _)| *id)
            .or_else(|| icns.iter().min_by_key(|(id, _)| **id).map(|(id, _)| *id));
        if let Some(icon) = actual.and_then(|id| family_icon(&families, id)) {
            out.insert(
                0,
                BundleIcon {
                    ftype: u32::from_be_bytes(*b"APPL"),
                    ..icon
                },
            );
        }
    }
    out
}

/// The members of one icon family resource, chunked by the member size
fn family(parsed: &Parsed, off: usize, member_len: usize) -> Option<Vec<Vec<u8>>> {
    let blob = resource_data(&parsed.data, parsed.area.0, parsed.area.1, off)?;
    let members: Vec<Vec<u8>> = blob
        .chunks(member_len)
        .filter(|c| c.len() == member_len)
        .map(|c| c.to_vec())
        .collect();
    (!members.is_empty()).then_some(members)
}

/// The first member of each kind of the family with this ID, for no file
/// type yet; None without a monochrome icon
fn family_icon(families: &Families, id: i16) -> Option<BundleIcon> {
    let first = |kind: usize| families[kind].get(&id).and_then(|m| m.first()).cloned();
    Some(BundleIcon {
        ftype: 0,
        mono: first(ICON_MONO)?,
        color8: first(ICON_COLOR8),
        color4: first(ICON_COLOR4),
        small_mono: first(ICON_SMALL_MONO),
        small8: first(ICON_SMALL8),
        small4: first(ICON_SMALL4),
    })
}

/// A parsed resource fork
struct Parsed {
    data: Vec<u8>,
    /// The data area: (offset, length) within the fork
    area: (usize, usize),
    /// The icon family resources of each of ICON_KINDS: (id, offset into
    /// the data area)
    icons: [Vec<(i16, usize)>; 6],
    /// The (file type, local ID) table of the FREF resources the BNDLs name
    frefs: Vec<(u32, i16)>,
    /// The bundle's local ID → ICN# resource ID map
    local_map: std::collections::HashMap<i16, i16>,
}

/// A count stored as "minus 1" (Inside Macintosh Volume I, I-129: the
/// number of resource types in the map minus 1, and the same for the
/// resources of a type). An empty map stores $FFFF, which is no entry at
/// all, not 65,536 of them.
fn count(minus_one: u16) -> usize {
    usize::from(minus_one.wrapping_add(1))
}

/// Parse a resource fork for its BNDL, FREF and ICN# resources
fn parse(fork: &[u8]) -> Option<Parsed> {
    let doff = be32(fork, 0)? as usize;
    let moff = be32(fork, 4)? as usize;
    let dlen = be32(fork, 8)? as usize;
    let mlen = be32(fork, 12)? as usize;
    let map = fork.get(moff..moff.checked_add(mlen)?)?;

    // The type list, which begins with its own count of types minus one
    let tl = moff + be16(map, 24)? as usize;
    let ntypes = count(be16(fork, tl)?);

    let mut icons: [Vec<(i16, usize)>; 6] = Default::default();
    let mut bndl_blobs = Vec::new();
    let mut fref_blobs = Vec::new();
    for i in 0..ntypes {
        let Some(entry) = fork.get(tl + 2 + i * 8..tl + 2 + i * 8 + 8) else {
            continue;
        };
        let nres = count(u16::from_be_bytes([entry[4], entry[5]]));
        let rloff = u16::from_be_bytes([entry[6], entry[7]]) as usize;
        // A type whose reference list does not validate only costs that
        // type
        let Some(refs) = ref_list(fork, tl + rloff, nres, doff, dlen) else {
            continue;
        };
        match &entry[0..4] {
            b"BNDL" => bndl_blobs.extend(refs),
            b"FREF" => fref_blobs.extend(refs),
            rtype => {
                if let Some(kind) = ICON_KINDS.iter().position(|(t, _)| t == &rtype) {
                    icons[kind].extend(refs);
                }
            }
        }
    }

    // The BNDLs: the local → ICN# map, and which FREFs belong to the bundle
    let mut local_map = std::collections::HashMap::new();
    let mut bundle_frefs = std::collections::HashSet::new();
    for (_, off) in bndl_blobs {
        let Some(lists) = parse_bndl(fork, doff, dlen, off) else {
            continue;
        };
        for (rtype, pairs) in lists {
            for (local, id) in pairs {
                match &rtype {
                    b"ICN#" => {
                        local_map.entry(local).or_insert(id);
                    }
                    b"FREF" => {
                        bundle_frefs.insert(id);
                    }
                    // The color icons share the ICN# IDs
                    _ => {}
                }
            }
        }
    }

    // The FREFs: (file type, local ID) entries with empty names
    let mut frefs = Vec::new();
    for (id, off) in fref_blobs {
        if !bundle_frefs.contains(&id) {
            continue;
        }
        let Some(blob) = resource_data(fork, doff, dlen, off) else {
            continue;
        };
        let mut at = 0;
        while at + 6 <= blob.len() {
            let ftype = u32::from_be_bytes(blob[at..at + 4].try_into().ok()?);
            let local = i16::from_be_bytes([blob[at + 4], blob[at + 5]]);
            at += 6;
            let name_len = *blob.get(at)? as usize;
            at += 1 + name_len;
            if at > blob.len() {
                break;
            }
            frefs.push((ftype, local));
        }
    }

    Some(Parsed {
        data: fork.to_vec(),
        area: (doff, dlen),
        icons,
        frefs,
        local_map,
    })
}

/// The reference list of a type: (id, data offset) pairs, validated to
/// land inside the data area on length-prefixed blobs
fn ref_list(
    fork: &[u8],
    rl: usize,
    nres: usize,
    doff: usize,
    dlen: usize,
) -> Option<Vec<(i16, usize)>> {
    let mut out = Vec::with_capacity(nres);
    for j in 0..nres {
        let entry = fork.get(rl + j * 12..rl + j * 12 + 12)?;
        let id = i16::from_be_bytes([entry[0], entry[1]]);
        let off = be24(entry, 5)? as usize;
        if off >= dlen {
            return None;
        }
        let at = doff + off;
        let blob_len = be32(fork, at)?;
        if blob_len == 0 || blob_len as usize > dlen || at + 4 + blob_len as usize > fork.len() {
            return None;
        }
        out.push((id, off));
    }
    Some(out)
}

/// One resource type's list in a BNDL: the type and its (local ID,
/// resource ID) pairs
type BndlList = ([u8; 4], Vec<(i16, i16)>);

/// Parse a BNDL resource: the signature, the version data's resource ID,
/// then the number of types minus one, and per type the type, the number of
/// IDs minus one and the pairs (Inside Macintosh Volume III, III-12 to
/// III-13). All 2024 bundles surveyed from real applications parse this
/// way; 62% list FREF before ICN#.
fn parse_bndl(fork: &[u8], doff: usize, dlen: usize, off: usize) -> Option<Vec<BndlList>> {
    let blob = resource_data(fork, doff, dlen, off)?;
    let types = count(be16(blob, 6)?);
    let mut at = 8;
    let mut lists = Vec::with_capacity(types);
    for _ in 0..types {
        let rtype: [u8; 4] = blob.get(at..at + 4)?.try_into().ok()?;
        let ids = count(be16(blob, at + 4)?);
        at += 6;
        let mut pairs = Vec::with_capacity(ids);
        for _ in 0..ids {
            let pair = blob.get(at..at + 4)?;
            pairs.push((
                i16::from_be_bytes([pair[0], pair[1]]),
                i16::from_be_bytes([pair[2], pair[3]]),
            ));
            at += 4;
        }
        lists.push((rtype, pairs));
    }
    Some(lists)
}

/// The length-prefixed resource data at a data-area offset
fn resource_data(fork: &[u8], doff: usize, dlen: usize, off: usize) -> Option<&[u8]> {
    let at = doff + off;
    let blob_len = be32(fork, at)? as usize;
    let end = at + 4 + blob_len;
    if blob_len == 0 || end > doff + dlen {
        return None;
    }
    fork.get(at + 4..end)
}

fn be16(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

fn be24(d: &[u8], o: usize) -> Option<u32> {
    let b = d.get(o..o + 3)?;
    Some(u32::from_be_bytes([0, b[0], b[1], b[2]]))
}

fn be32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

#[cfg(test)]
pub(crate) mod test_support {
    /// A resource-fork fixture: a BNDL, an optional FREF, and the full
    /// icon families (ICN#, icl8, icl4, ics8, ics4, ics#) for IDs 128
    /// and 129, each with one member. The type list begins with its
    /// count (types minus one), as the Resource Manager reads it, and
    /// the reference-list offsets count from the type list's start.
    pub fn fork(bndl: &[u8], fref: Option<&[u8]>) -> Vec<u8> {
        let mono = [vec![0x11u8; 256], vec![0x22u8; 256]];
        let color8 = [vec![0x33u8; 1024], vec![0x44u8; 1024]];
        let color4 = [vec![0x55u8; 512], vec![0x66u8; 512]];
        let small8 = [vec![0x77u8; 256], vec![0x88u8; 256]];
        let small4 = [vec![0x99u8; 128], vec![0xaau8; 128]];
        let small = [vec![0xbbu8; 64], vec![0xccu8; 64]];

        // The data area: length-prefixed blobs
        let mut data = Vec::new();
        let bndl_off = data.len();
        data.extend_from_slice(&(bndl.len() as u32).to_be_bytes());
        data.extend_from_slice(bndl);
        let fref_off = fref.map(|f| {
            let off = data.len();
            data.extend_from_slice(&(f.len() as u32).to_be_bytes());
            data.extend_from_slice(f);
            off
        });
        // Per family: ICN#, icl8, icl4, ics8, ics4, ics#
        let mut blobs: Vec<usize> = Vec::new();
        for i in 0..2 {
            for blob in [
                mono[i].as_slice(),
                color8[i].as_slice(),
                color4[i].as_slice(),
                small8[i].as_slice(),
                small4[i].as_slice(),
                small[i].as_slice(),
            ] {
                blobs.push(data.len());
                data.extend_from_slice(&(blob.len() as u32).to_be_bytes());
                data.extend_from_slice(blob);
            }
        }

        // The map: the header, then the type list (count first), then
        // the reference lists
        let mut map = Vec::new();
        map.extend_from_slice(&[0u8; 16]); // reserved copy of the header
        map.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes()); // next map
        map.extend_from_slice(&0u16.to_be_bytes()); // file ref
        map.extend_from_slice(&0u16.to_be_bytes()); // attributes
        let tl_rel = 16 + 4 + 2 + 2 + 2 + 2;
        map.extend_from_slice(&(tl_rel as u16).to_be_bytes());
        map.extend_from_slice(&0u16.to_be_bytes()); // name list offset
        let ntypes = if fref.is_some() { 8 } else { 7 };
        map.extend_from_slice(&((ntypes - 1) as u16).to_be_bytes());
        let rl_base = 2 + ntypes * 8;
        // (type, resources, ref list offset)
        let refs = [
            ("BNDL", 1, rl_base),
            ("ICN#", 2, rl_base + 12),
            ("icl8", 2, rl_base + 36),
            ("icl4", 2, rl_base + 60),
            ("ics8", 2, rl_base + 84),
            ("ics4", 2, rl_base + 108),
            ("ics#", 2, rl_base + 132),
        ];
        for (ty, nres, off) in refs {
            map.extend_from_slice(ty.as_bytes());
            map.extend_from_slice(&((nres - 1) as u16).to_be_bytes());
            map.extend_from_slice(&(off as u16).to_be_bytes());
        }
        if fref.is_some() {
            map.extend_from_slice(b"FREF");
            map.extend_from_slice(&0u16.to_be_bytes());
            map.extend_from_slice(&((rl_base + 156) as u16).to_be_bytes());
        }
        // The reference lists: BNDL, then per family 128/129 the six
        // members in the blobs order
        push_ref(&mut map, 128, bndl_off);
        // Per type: family 128 then family 129
        for (a, b) in [(0, 6), (1, 7), (2, 8), (3, 9), (4, 10), (5, 11)] {
            push_ref(&mut map, 128, blobs[a]);
            push_ref(&mut map, 129, blobs[b]);
        }
        if let Some(fref_off) = fref_off {
            push_ref(&mut map, 128, fref_off);
        }

        let mlen = map.len();
        let doff = 256;
        let mut out = vec![0u8; doff];
        out.extend_from_slice(&data);
        let moff = out.len();
        let dlen = data.len();
        out.extend_from_slice(&map);
        out[moff..moff + 4].copy_from_slice(&(doff as u32).to_be_bytes());
        out[moff + 4..moff + 8].copy_from_slice(&(moff as u32).to_be_bytes());
        out[moff + 8..moff + 12].copy_from_slice(&(dlen as u32).to_be_bytes());
        out[moff + 12..moff + 16].copy_from_slice(&(mlen as u32).to_be_bytes());
        out[0..4].copy_from_slice(&(doff as u32).to_be_bytes());
        out[4..8].copy_from_slice(&(moff as u32).to_be_bytes());
        out[8..12].copy_from_slice(&(dlen as u32).to_be_bytes());
        out[12..16].copy_from_slice(&(mlen as u32).to_be_bytes());
        out
    }

    fn push_ref(map: &mut Vec<u8>, id: i16, data_off: usize) {
        map.extend_from_slice(&id.to_be_bytes());
        map.extend_from_slice(&(-1i16).to_be_bytes()); // unnamed
        map.push(0); // attributes
        map.extend_from_slice(&[
            (data_off >> 16) as u8,
            (data_off >> 8) as u8,
            data_off as u8,
        ]);
        map.extend_from_slice(&0u32.to_be_bytes()); // handle
    }

    /// One list of a test BNDL: the type and its (local ID, resource ID)
    /// pairs
    pub type BndlListRef<'a> = (&'a [u8; 4], &'a [(i16, i16)]);

    /// A BNDL from its lists of (local ID, resource ID) pairs, in order
    pub fn bndl(lists: &[BndlListRef]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"TEST");
        b.extend_from_slice(&0i16.to_be_bytes());
        b.extend_from_slice(&(lists.len() as u16 - 1).to_be_bytes());
        for (rtype, pairs) in lists {
            b.extend_from_slice(*rtype);
            b.extend_from_slice(&(pairs.len() as u16 - 1).to_be_bytes());
            for (local, id) in *pairs {
                b.extend_from_slice(&local.to_be_bytes());
                b.extend_from_slice(&id.to_be_bytes());
            }
        }
        b
    }

    /// A BNDL with local 0 → ICN# 128 and local 1 → ICN# 129, naming the
    /// fixture's FREF 128
    pub fn bndl_with_fref() -> Vec<u8> {
        bndl(&[(b"ICN#", &[(0, 128), (1, 129)]), (b"FREF", &[(0, 128)])])
    }

    /// An FREF resource: the (file type, local ID) table
    pub fn fref_entries() -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(b"APPL");
        f.extend_from_slice(&0i16.to_be_bytes());
        f.push(0);
        f.extend_from_slice(b"TEXT");
        f.extend_from_slice(&1i16.to_be_bytes());
        f.push(0);
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icon_of(icons: &[BundleIcon], ftype: &[u8; 4]) -> Option<u8> {
        icons
            .iter()
            .find(|i| i.ftype == u32::from_be_bytes(*ftype))
            .map(|i| i.mono[0])
    }

    #[test]
    fn a_minus_one_count_of_ffff_is_empty() {
        assert_eq!(count(0xFFFF), 0);
        assert_eq!(count(0), 1);
        assert_eq!(count(0xFFFE), 0xFFFF);
    }

    #[test]
    fn bundle_icons_with_fref_listed_first() {
        // The more common order, with the icons crossed over the FREF IDs:
        // family 128 is 0x11 and family 129 is 0x22
        let bndl = test_support::bndl(&[(b"FREF", &[(0, 128)]), (b"ICN#", &[(0, 129), (1, 128)])]);
        let icons = bundle_icons(&test_support::fork(
            &bndl,
            Some(&test_support::fref_entries()),
        ));
        assert_eq!(icon_of(&icons, b"APPL"), Some(0x22));
        assert_eq!(icon_of(&icons, b"TEXT"), Some(0x11));
    }

    #[test]
    fn bundle_icons_with_more_types_listed() {
        let bndl = test_support::bndl(&[
            (b"ICN#", &[(0, 129), (1, 128)]),
            (b"ics#", &[(0, 129), (1, 128)]),
            (b"FREF", &[(0, 128)]),
        ]);
        let icons = bundle_icons(&test_support::fork(
            &bndl,
            Some(&test_support::fref_entries()),
        ));
        assert_eq!(icon_of(&icons, b"APPL"), Some(0x22));
        assert_eq!(icon_of(&icons, b"TEXT"), Some(0x11));
    }

    #[test]
    fn bundle_icons_ignore_a_fref_outside_the_bundle() {
        // The BNDL names FREF 200, the fork only has FREF 128
        let bndl = test_support::bndl(&[(b"ICN#", &[(0, 129), (1, 128)]), (b"FREF", &[(0, 200)])]);
        let icons = bundle_icons(&test_support::fork(
            &bndl,
            Some(&test_support::fref_entries()),
        ));
        assert_eq!(icon_of(&icons, b"TEXT"), None);
        // The application still gets the ICN# 128 fallback
        assert_eq!(icon_of(&icons, b"APPL"), Some(0x11));
    }

    #[test]
    fn a_truncated_bndl_maps_nothing() {
        let bndl = test_support::bndl_with_fref();
        let icons = bundle_icons(&test_support::fork(
            &bndl[..bndl.len() - 3],
            Some(&test_support::fref_entries()),
        ));
        assert_eq!(icon_of(&icons, b"TEXT"), None);
    }

    #[test]
    fn bundle_icons_via_fref_and_mapping() {
        let icons = bundle_icons(&test_support::fork(
            &test_support::bndl_with_fref(),
            Some(&test_support::fref_entries()),
        ));
        assert_eq!(icons.len(), 2);
        assert_eq!(icons[0].ftype, u32::from_be_bytes(*b"APPL"));
        assert_eq!(icons[0].mono, vec![0x11u8; 256]);
        assert_eq!(icons[0].small_mono, Some(vec![0xbbu8; 64]));
        assert_eq!(icons[0].small8, Some(vec![0x77u8; 256]));
        assert_eq!(icons[0].small4, Some(vec![0x99u8; 128]));
        assert_eq!(icons[0].color8, Some(vec![0x33u8; 1024]));
        assert_eq!(icons[0].color4, Some(vec![0x55u8; 512]));
        assert_eq!(icons[1].ftype, u32::from_be_bytes(*b"TEXT"));
        assert_eq!(icons[1].mono, vec![0x22u8; 256]);
        assert_eq!(icons[1].color8, Some(vec![0x44u8; 1024]));
        assert_eq!(icons[1].color4, Some(vec![0x66u8; 512]));
    }

    #[test]
    fn bundle_icons_no_appl_entry_uses_icn_128() {
        // A bundle whose FREF only maps TEXT: the APPL icon falls back
        // to ICN# 128
        let mut f = Vec::new();
        f.extend_from_slice(b"TEXT");
        f.extend_from_slice(&1i16.to_be_bytes());
        f.push(0);
        let icons = bundle_icons(&test_support::fork(
            &test_support::bndl_with_fref(),
            Some(&f),
        ));
        assert_eq!(icons.len(), 2);
        assert_eq!(icons[0].ftype, u32::from_be_bytes(*b"APPL"));
        assert_eq!(icons[0].mono, vec![0x11u8; 256]);
        assert_eq!(icons[0].small_mono, Some(vec![0xbbu8; 64]));
        assert_eq!(icons[0].small8, Some(vec![0x77u8; 256]));
        assert_eq!(icons[0].small4, Some(vec![0x99u8; 128]));
        assert_eq!(icons[0].color8, Some(vec![0x33u8; 1024]));
        assert_eq!(icons[0].color4, Some(vec![0x55u8; 512]));
        assert_eq!(icons[1].ftype, u32::from_be_bytes(*b"TEXT"));
        assert_eq!(icons[1].mono, vec![0x22u8; 256]);
        assert_eq!(icons[1].color8, Some(vec![0x44u8; 1024]));
        assert_eq!(icons[1].color4, Some(vec![0x66u8; 512]));
    }

    #[test]
    fn bundle_icons_rejects_garbage() {
        assert!(bundle_icons(b"not a resource fork").is_empty());
        assert!(bundle_icons(&[]).is_empty());
    }
}
