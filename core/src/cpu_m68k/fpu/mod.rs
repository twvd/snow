pub mod alu;
pub mod instruction;
pub mod math;
pub mod ops_branch;
pub mod ops_generic;
pub mod regs;
pub mod storage;
pub mod trig;

use arpfloat::{RoundingMode, Semantics};

/// 6888x/68040 single precision float semantics
pub const SEMANTICS_SINGLE: Semantics = Semantics::new(8, 24, RoundingMode::NearestTiesToEven);

/// 6888x/68040 double precision float semantics
pub const SEMANTICS_DOUBLE: Semantics = Semantics::new(11, 53, RoundingMode::NearestTiesToEven);

/// 6888x/68040 extended precision float semantics
pub const SEMANTICS_EXTENDED: Semantics = Semantics::new(15, 64, RoundingMode::NearestTiesToEven);
