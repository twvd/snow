//! Macintosh resource fork parsing for the desktop database.
//!
//! The AFP desktop-database calls serve the BNDL → ICN# icon mapping of
//! APPL files. That mapping lives in the file's resource fork, which
//! the MetaStore exposes inside AppleDouble sidecars and AppleSingle
//! containers.
//!
//! The resource map layout is the one the Resource Manager reads
//! (Inside Macintosh): the fork header gives the map position; the map
//! points at the type list, which itself begins with a count and whose
//! entries point at per-type reference lists of (ID, name offset,
//! attributes, 24-bit data offset, handle) entries.
//!
//! The icon mapping is the Finder Interface's bundle format: the BNDL
//! resource holds the signature, the bundle ID, a counted array of
//! (type, local ID) entries, then the local-ID → ICN#-resource-ID
//! mapping pairs; the FREF resources hold the (file type, local ID)
//! table. A file type's icon is the first member of the ICN# resource
//! whose ID the mapping assigns to the type's local ID.

/// One ICN# member: a 32x32 monochrome icon (128 bytes of bitmap
/// followed by 128 bytes of mask)
const ICON_LEN: usize = 256;
/// One icl8 member: a 32x32 8-bit color pixmap
pub const ICL8_LEN: usize = 1024;
/// One icl4 member: a 32x32 4-bit color pixmap
pub const ICL4_LEN: usize = 512;
/// One ics8 member: a 16x16 8-bit color pixmap
const ICS8_LEN: usize = 256;
/// One ics4 member: a 16x16 4-bit color pixmap
const ICS4_LEN: usize = 128;
/// One ics# member: a 16x16 monochrome icon (32 bytes of bitmap plus
/// 32 bytes of mask)
const ICS_LEN: usize = 64;

/// An icon a BNDL maps to a file type
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
    let mut icns: std::collections::HashMap<i16, Vec<Vec<u8>>> = std::collections::HashMap::new();
    for (id, off) in &parsed.icns {
        if let Some(members) = family(&parsed, *off, ICON_LEN) {
            icns.insert(*id, members);
        }
    }
    let mut icl8s: std::collections::HashMap<i16, Vec<Vec<u8>>> = std::collections::HashMap::new();
    for (id, off) in &parsed.icl8s {
        if let Some(members) = family(&parsed, *off, ICL8_LEN) {
            icl8s.insert(*id, members);
        }
    }
    let mut icl4s: std::collections::HashMap<i16, Vec<Vec<u8>>> = std::collections::HashMap::new();
    for (id, off) in &parsed.icl4s {
        if let Some(members) = family(&parsed, *off, ICL4_LEN) {
            icl4s.insert(*id, members);
        }
    }
    let mut ics8s: std::collections::HashMap<i16, Vec<Vec<u8>>> = std::collections::HashMap::new();
    for (id, off) in &parsed.ics8s {
        if let Some(members) = family(&parsed, *off, ICS8_LEN) {
            ics8s.insert(*id, members);
        }
    }
    let mut ics4s: std::collections::HashMap<i16, Vec<Vec<u8>>> = std::collections::HashMap::new();
    for (id, off) in &parsed.ics4s {
        if let Some(members) = family(&parsed, *off, ICS4_LEN) {
            ics4s.insert(*id, members);
        }
    }
    let mut icss: std::collections::HashMap<i16, Vec<Vec<u8>>> = std::collections::HashMap::new();
    for (id, off) in &parsed.icss {
        if let Some(members) = family(&parsed, *off, ICS_LEN) {
            icss.insert(*id, members);
        }
    }
    if icns.is_empty() {
        return Vec::new();
    }

    // The file types the bundle maps, in order: the BNDL's own
    // (type, local ID) entries first, then every FREF's entry. The
    // BNDL's 'FREF' and 'ICN#' entries are bundle-membership records,
    // not file-type mappings, so they are skipped; the FREF resources
    // carry the (file type, local ID) table the Finder uses.
    let mut types: Vec<(u32, i16)> = Vec::new();
    let mut seen_types: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (entries, _) in &parsed.bndls {
        for (ftype, local) in entries {
            if *ftype == u32::from_be_bytes(*b"FREF") || *ftype == u32::from_be_bytes(*b"ICN#") {
                continue;
            }
            if seen_types.insert(*ftype) {
                types.push((*ftype, *local));
            }
        }
    }
    for (ftype, local) in &parsed.frefs {
        if seen_types.insert(*ftype) {
            types.push((*ftype, *local));
        }
    }

    let mut out: Vec<BundleIcon> = Vec::new();
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (ftype, local) in types {
        let Some(icon) = resolve_icon(
            &icns,
            &icl8s,
            &icl4s,
            &ics8s,
            &ics4s,
            &icss,
            &parsed.local_map,
            local,
        ) else {
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
        if let Some(actual) = actual {
            let icon = family_icon(&icns, &icl8s, &icl4s, &ics8s, &ics4s, &icss, actual);
            out.insert(
                0,
                BundleIcon {
                    ftype: u32::from_be_bytes(*b"APPL"),
                    mono: icon.0,
                    color8: icon.1,
                    color4: icon.2,
                    small_mono: icon.3,
                    small8: icon.4,
                    small4: icon.5,
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

/// The bitmaps of one icon family: the monochrome icon and the large
/// and small color pixmaps, each optional when the family does not
/// carry it
pub(crate) type IconFamily = (
    Vec<u8>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

/// An icon family with the file type it maps to
pub(crate) type IconFamilyEntry = (u32, IconFamily);

/// The first member of a family across the monochrome, 8-bit and 4-bit
/// families (large and small) with the same ID
fn family_icon(
    icns: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    icl8s: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    icl4s: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    ics8s: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    ics4s: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    icss: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    id: i16,
) -> IconFamily {
    let mono = icns
        .get(&id)
        .and_then(|m| m.first())
        .expect("the family exists");
    (
        mono.clone(),
        icl8s.get(&id).and_then(|m| m.first()).cloned(),
        icl4s.get(&id).and_then(|m| m.first()).cloned(),
        icss.get(&id).and_then(|m| m.first()).cloned(),
        ics8s.get(&id).and_then(|m| m.first()).cloned(),
        ics4s.get(&id).and_then(|m| m.first()).cloned(),
    )
}

/// Resolve a BNDL local icon ID to the family's icons: the BNDL's
/// local → ICN# map gives the family ID; without a mapping the local
/// ID names the family directly
#[allow(clippy::too_many_arguments)] // one map per icon family
fn resolve_icon(
    icns: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    icl8s: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    icl4s: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    ics8s: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    ics4s: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    icss: &std::collections::HashMap<i16, Vec<Vec<u8>>>,
    local_map: &std::collections::HashMap<i16, i16>,
    local: i16,
) -> Option<BundleIcon> {
    let actual = local_map.get(&local).copied().unwrap_or(local);
    if !icns.contains_key(&actual) {
        return None;
    }
    let (mono, color8, color4, small_mono, small8, small4) =
        family_icon(icns, icl8s, icl4s, ics8s, ics4s, icss, actual);
    Some(BundleIcon {
        ftype: 0,
        mono,
        color8,
        color4,
        small_mono,
        small8,
        small4,
    })
}

/// A parsed BNDL resource: the counted (type, local ID) entries and
/// the raw local → ICN# pairs that follow
type BndlParts = (Vec<(u32, i16)>, Vec<(i16, i16)>);

/// A BNDL resource with its local → ICN# map
type Bndl = (Vec<(u32, i16)>, std::collections::HashMap<i16, i16>);

/// A parsed resource fork
struct Parsed {
    data: Vec<u8>,
    /// The data area: (offset, length) within the fork
    area: (usize, usize),
    /// ICN# resources: (id, offset into the data area)
    icns: Vec<(i16, usize)>,
    /// icl8 resources: (id, offset into the data area)
    icl8s: Vec<(i16, usize)>,
    /// icl4 resources: (id, offset into the data area)
    icl4s: Vec<(i16, usize)>,
    /// ics8 resources: (id, offset into the data area)
    ics8s: Vec<(i16, usize)>,
    /// ics4 resources: (id, offset into the data area)
    ics4s: Vec<(i16, usize)>,
    /// ics# resources: (id, offset into the data area)
    icss: Vec<(i16, usize)>,
    /// BNDL resources: (entries, local → ICN# ID pairs)
    bndls: Vec<Bndl>,
    /// The (file type, local ID) table from every FREF resource
    frefs: Vec<(u32, i16)>,
    /// The bundle's local ID → ICN# resource ID map
    local_map: std::collections::HashMap<i16, i16>,
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
    let ntypes = be16(fork, tl)? as usize + 1;

    let mut icns = Vec::new();
    let mut icl8s = Vec::new();
    let mut icl4s = Vec::new();
    let mut ics8s = Vec::new();
    let mut ics4s = Vec::new();
    let mut icss = Vec::new();
    let mut bndl_blobs = Vec::new();
    let mut fref_blobs = Vec::new();
    for i in 0..ntypes {
        let Some(entry) = fork.get(tl + 2 + i * 8..tl + 2 + i * 8 + 8) else {
            continue;
        };
        let nres = u16::from_be_bytes([entry[4], entry[5]]) as usize + 1;
        let rloff = u16::from_be_bytes([entry[6], entry[7]]) as usize;
        // A type whose reference list does not validate only costs that
        // type
        let Some(refs) = ref_list(fork, tl + rloff, nres, doff, dlen) else {
            continue;
        };
        match &entry[0..4] {
            b"ICN#" => icns.extend(refs),
            b"icl8" => icl8s.extend(refs),
            b"icl4" => icl4s.extend(refs),
            b"ics8" => ics8s.extend(refs),
            b"ics4" => ics4s.extend(refs),
            b"ics#" => icss.extend(refs),
            b"BNDL" => bndl_blobs.extend(refs),
            b"FREF" => fref_blobs.extend(refs),
            _ => {}
        }
    }

    // The BNDLs: the counted entries and the following local → ICN#
    // pairs; the pairs of the bundle's mapping are the ones whose
    // second value is an ICN# resource ID present in the file
    let mut bndls = Vec::new();
    let mut local_map = std::collections::HashMap::new();
    for (_, off) in bndl_blobs {
        let Some((entries, pairs)) = parse_bndl(fork, doff, dlen, off) else {
            continue;
        };
        let icn_ids: std::collections::HashSet<i16> = icns.iter().map(|(id, _)| *id).collect();
        for (local, actual) in pairs {
            if icn_ids.contains(&actual) {
                local_map.entry(local).or_insert(actual);
            }
        }
        bndls.push((entries, local_map.clone()));
    }

    // The FREFs: (file type, local ID) entries with empty names
    let mut frefs = Vec::new();
    for (_, off) in fref_blobs {
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
        icns,
        icl8s,
        icl4s,
        ics8s,
        ics4s,
        icss,
        bndls,
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

/// Parse a BNDL resource: the signature, bundle ID, the counted
/// (type, local ID) entries, then the raw (u16, u16) pairs that follow
/// (the bundle's local ID → ICN# resource ID mapping)
fn parse_bndl(fork: &[u8], doff: usize, dlen: usize, off: usize) -> Option<BndlParts> {
    let blob = resource_data(fork, doff, dlen, off)?;
    if blob.len() < 8 {
        return None;
    }
    let count = u16::from_be_bytes([blob[6], blob[7]]) as usize;
    let mut entries = Vec::new();
    for k in 0..count {
        let e = blob.get(8 + k * 6..8 + k * 6 + 6)?;
        if !e[0..4].iter().all(|b| (32..127).contains(b)) {
            return None;
        }
        let ftype = u32::from_be_bytes(e[0..4].try_into().ok()?);
        let local = i16::from_be_bytes([e[4], e[5]]);
        entries.push((ftype, local));
    }
    let rest = &blob[8 + count * 6..];
    let pairs: Vec<(i16, i16)> = rest
        .chunks_exact(4)
        .map(|p| {
            (
                i16::from_be_bytes([p[0], p[1]]),
                i16::from_be_bytes([p[2], p[3]]),
            )
        })
        .collect();
    Some((entries, pairs))
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

    /// A BNDL whose entries map APPL → ICN# 128 and TEXT → ICN# 129
    /// directly
    pub fn bndl_direct() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"TEST");
        b.extend_from_slice(&128i16.to_be_bytes());
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(b"APPL");
        b.extend_from_slice(&128i16.to_be_bytes());
        b.extend_from_slice(b"TEXT");
        b.extend_from_slice(&129i16.to_be_bytes());
        b
    }

    /// A BNDL whose entries are bundle-membership records, with the
    /// (type → local ID) table in an FREF and the local → ICN# mapping
    /// in the BNDL's trailing pairs
    pub fn bndl_with_fref() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"TEST");
        b.extend_from_slice(&0i16.to_be_bytes());
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(b"FREF");
        b.extend_from_slice(&128i16.to_be_bytes());
        b.extend_from_slice(&0i16.to_be_bytes());
        b.extend_from_slice(&128i16.to_be_bytes());
        b.extend_from_slice(&1i16.to_be_bytes());
        b.extend_from_slice(&129i16.to_be_bytes());
        b
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

mod tests {
    #[allow(unused_imports)]
    // only used by the #[test] functions, which lib builds do not compile
    use super::*;

    #[test]
    fn bundle_icons_direct_entries() {
        let icons = bundle_icons(&test_support::fork(&test_support::bndl_direct(), None));
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
