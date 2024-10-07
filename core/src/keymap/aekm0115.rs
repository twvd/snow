//! Apple extended keyboard M0115
//!
//! ,---.   .---------------. ,---------------. ,---------------. ,-----------.             ,---.
//! |Esc|   |F1 |F2 |F3 |F4 | |F5 |F6 |F7 |F8 | |F9 |F10|F11|F12| |PrS|ScL|Pau|             |Pwr|
//! `---'   `---------------' `---------------' `---------------' `-----------'             `---'
//! ,-----------------------------------------------------------. ,-----------. ,---------------.
//! |  `|  1|  2|  3|  4|  5|  6|  7|  8|  9|  0|  -|  =|Backspa| |Ins|Hom|PgU| |NmL|  =|  /|  *|
//! |-----------------------------------------------------------| |-----------| |---------------|
//! |Tab  |  Q|  W|  E|  R|  T|  Y|  U|  I|  O|  P|  [|  ]|  \  | |Del|End|PgD| |  7|  8|  9|  -|
//! |-----------------------------------------------------------| `-----------' |---------------|
//! |CapsLo|  A|  S|  D|  F|  G|  H|  J|  K|  L|  ;|  '|  Return|               |  4|  5|  6|  +|
//! |-----------------------------------------------------------|     ,---.     |---------------|
//! |Shift   |  Z|  X|  C|  V|  B|  N|  M|  ,|  ,|  /|Shift     |     |Up |     |  1|  2|  3|   |
//! |-----------------------------------------------------------| ,-----------. |-----------|Ent|
//! |Ctrl |Opt | Cmd |        Space            | Cmd |Opt |Ctrl | |Lef|Dow|Rig| |      0|  .|   |
//! `-----------------------------------------------------------' `-----------' `---------------'
//!                    
//! ,---.   .---------------. ,---------------. ,---------------. ,-----------.             ,---.
//! | 35|   | 7A| 78| 63| 76| | 60| 61| 62| 64| | 65| 6D| 67| 6F| | 69| 6B| 71|             | 7F|
//! `---'   `---------------' `---------------' `---------------' `-----------'             `---'
//! ,-----------------------------------------------------------. ,-----------. ,---------------.
//! | 32| 12| 13| 14| 15| 17| 16| 1A| 1C| 19| 1D| 1B| 18|   33  | | 72| 73| 74| | 47| 51| 4B| 43|
//! |-----------------------------------------------------------| |-----------| |---------------|
//! |  30 | 0C| 0D| 0E| 0F| 11| 10| 20| 22| 1F| 23| 21| 1E|  2A | | 75| 77| 79| | 59| 5B| 5C| 4E|
//! |-----------------------------------------------------------| `-----------' |---------------|
//! |  39  | 00| 01| 02| 03| 05| 04| 26| 28| 25| 29| 27|   24   |               | 56| 57| 58| 45|
//! |-----------------------------------------------------------|     ,---.     |---------------|
//! |   38   | 06| 07| 08| 09| 0B| 2D| 2E| 2B| 2F| 2C|    7B    |     | 3E|     | 53| 54| 55|   |
//! |-----------------------------------------------------------| ,-----------. |-----------| 4C|
//! |  36 | 3A |  37 |           31            |  37 | 7C |  7D | | 3B| 3D| 3C| |    52 | 41|   |
//! `-----------------------------------------------------------' `-----------' `---------------'

use super::Scancode;

pub(super) fn translate(sc: Scancode) -> Option<Scancode> {
    // Since this is used as input/'universal' keymap, just return the same.
    Some(sc)
}