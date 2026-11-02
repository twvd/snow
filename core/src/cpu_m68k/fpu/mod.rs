pub mod alu;
pub mod arith;
pub mod instruction;
pub mod math;
pub mod ops_branch;
pub mod ops_generic;
pub mod regs;
pub mod storage;
pub mod timings;
pub mod trig;

use arpfloat::{RoundingMode, Semantics};

/// 6888x/68040 single precision float semantics
pub const SEMANTICS_SINGLE: Semantics = Semantics::new(8, 24, RoundingMode::NearestTiesToEven);

/// 6888x/68040 double precision float semantics
pub const SEMANTICS_DOUBLE: Semantics = Semantics::new(11, 53, RoundingMode::NearestTiesToEven);

/// 6888x/68040 extended precision float semantics, with the 17-bit exponent of
/// the ALU and its temporary registers (MC68881UM 6.1.5), which also holds the
/// whole range of denormalized numbers
pub const SEMANTICS_EXTENDED: Semantics = Semantics::new(17, 64, RoundingMode::NearestTiesToEven);
