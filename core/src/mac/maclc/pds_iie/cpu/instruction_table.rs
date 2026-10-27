use super::instruction::{AddressingMode, InstructionDef, InstructionType};

macro_rules! instr {
    ($mnemonic:expr, $mode:expr, $typ:expr, $len:expr) => {
        InstructionDef {
            mnemonic: $mnemonic,
            mode: $mode,
            instr_type: $typ,
            len: $len,
        }
    };
}

pub const INSTRUCTION_TABLE: [InstructionDef; 256] = [
    // 0x00 - BRK
    // ....01.., 1 bytes, 8-e cycles
    instr!("BRK", AddressingMode::Implied, InstructionType::BRK, 1),
    // 0x01 - ORA ($10,X)
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "ORA (@,X)",
        AddressingMode::DirectXPtr16,
        InstructionType::ORA,
        2
    ),
    // 0x02 - COP #$12
    // ....01.., 2 bytes, 8-e cycles
    instr!(
        "COP #@",
        AddressingMode::Immediate8,
        InstructionType::COP,
        2
    ),
    // 0x03 - ORA $32,S
    // m.....m., 2 bytes, 5-m cycles
    instr!("ORA @,S", AddressingMode::StackS, InstructionType::ORA, 2),
    // 0x04 - TSB $10
    // ......m., 2 bytes, 7-2*m+w cycles
    instr!("TSB @", AddressingMode::Direct, InstructionType::TSB, 2),
    // 0x05 - ORA $10
    // m.....m., 2 bytes, 4-m+w cycles
    instr!("ORA @", AddressingMode::Direct, InstructionType::ORA, 2),
    // 0x06 - ASL $10
    // m.....mm, 2 bytes, 7-2*m+w cycles
    instr!("ASL @", AddressingMode::Direct, InstructionType::ASL, 2),
    // 0x07 - ORA [$10]
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "ORA [@]",
        AddressingMode::DirectPtr24,
        InstructionType::ORA,
        2
    ),
    // 0x08 - PHP
    // ........, 1 bytes, 3 cycles
    instr!("PHP", AddressingMode::Implied, InstructionType::PHP, 1),
    // 0x09 - ORA #$54
    // m.....m., 3-m bytes, 3-m cycles
    instr!(
        "ORA #@",
        AddressingMode::ImmediateM,
        InstructionType::ORA,
        3
    ),
    // 0x0A - ASL
    // m.....mm, 1 bytes, 2 cycles
    instr!("ASL", AddressingMode::Accumulator, InstructionType::ASL, 1),
    // 0x0B - PHD
    // ........, 1 bytes, 4 cycles
    instr!("PHD", AddressingMode::Implied, InstructionType::PHD, 1),
    // 0x0C - TSB $9876
    // ......m., 3 bytes, 8-2*m cycles
    instr!("TSB @", AddressingMode::Absolute, InstructionType::TSB, 3),
    // 0x0D - ORA $9876
    // m.....m., 3 bytes, 5-m cycles
    instr!("ORA @", AddressingMode::Absolute, InstructionType::ORA, 3),
    // 0x0E - ASL $9876
    // m.....mm, 3 bytes, 8-2*m cycles
    instr!("ASL @", AddressingMode::Absolute, InstructionType::ASL, 3),
    // 0x0F - ORA $FEDBCA
    // m.....m., 4 bytes, 6-m cycles
    instr!("ORA @", AddressingMode::Long, InstructionType::ORA, 4),
    // 0x10 - BPL LABEL
    // ........, 2 bytes, 2+t+t*e*p cycles
    instr!("BPL @", AddressingMode::Relative8, InstructionType::BPL, 2),
    // 0x11 - ORA ($10),Y
    // m.....m., 2 bytes, 7-m+w-x+x*p cycles
    instr!(
        "ORA (@),Y",
        AddressingMode::DirectPtr16Y,
        InstructionType::ORA,
        2
    ),
    // 0x12 - ORA ($10)
    // m.....m., 2 bytes, 6-m+w cycles
    instr!(
        "ORA (@)",
        AddressingMode::DirectPtr16,
        InstructionType::ORA,
        2
    ),
    // 0x13 - ORA ($32,S),Y
    // m.....m., 2 bytes, 8-m cycles
    instr!(
        "ORA (@,S),Y",
        AddressingMode::StackSPtr16Y,
        InstructionType::ORA,
        2
    ),
    // 0x14 - TRB $10
    // ......m., 2 bytes, 7-2*m+w cycles
    instr!("TRB @", AddressingMode::Direct, InstructionType::TRB, 2),
    // 0x15 - ORA $10,X
    // m.....m., 2 bytes, 5-m+w cycles
    instr!("ORA @,X", AddressingMode::DirectX, InstructionType::ORA, 2),
    // 0x16 - ASL $10,X
    // m.....mm, 2 bytes, 8-2*m+w cycles
    instr!("ASL @,X", AddressingMode::DirectX, InstructionType::ASL, 2),
    // 0x17 - ORA [$10],Y
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "ORA [@],Y",
        AddressingMode::DirectPtr24Y,
        InstructionType::ORA,
        2
    ),
    // 0x18 - CLC
    // .......0, 1 bytes, 2 cycles
    instr!("CLC", AddressingMode::Implied, InstructionType::CLC, 1),
    // 0x19 - ORA $9876,Y
    // m.....m., 3 bytes, 6-m-x+x*p cycles
    instr!(
        "ORA @,Y",
        AddressingMode::AbsoluteY,
        InstructionType::ORA,
        3
    ),
    // 0x1A - INC
    // m.....m., 1 bytes, 2 cycles
    instr!("INC", AddressingMode::Accumulator, InstructionType::INC, 1),
    // 0x1B - TCS
    // ........, 1 bytes, 2 cycles
    instr!("TCS", AddressingMode::Implied, InstructionType::TCS, 1),
    // 0x1C - TRB $9876
    // ......m., 3 bytes, 8-2*m cycles
    instr!("TRB @", AddressingMode::Absolute, InstructionType::TRB, 3),
    // 0x1D - ORA $9876,X
    // m.....m., 3 bytes, 6-m-x+x*p cycles
    instr!(
        "ORA @,X",
        AddressingMode::AbsoluteX,
        InstructionType::ORA,
        3
    ),
    // 0x1E - ASL $9876,X
    // m.....mm, 3 bytes, 9-2*m cycles
    instr!(
        "ASL @,X",
        AddressingMode::AbsoluteX,
        InstructionType::ASL,
        3
    ),
    // 0x1F - ORA $FEDCBA,X
    // m.....m., 4 bytes, 6-m cycles
    instr!("ORA @,X", AddressingMode::LongX, InstructionType::ORA, 4),
    // 0x20 - JSR $1234
    // ........, 3 bytes, 6 cycles
    instr!("JSR @", AddressingMode::Absolute, InstructionType::JSR, 3),
    // 0x21 - AND ($10,X)
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "AND (@,X)",
        AddressingMode::DirectXPtr16,
        InstructionType::AND,
        2
    ),
    // 0x22 - JSL $123456
    // ........, 4 bytes, 8 cycles
    instr!("JSL @", AddressingMode::Long, InstructionType::JSL, 4),
    // 0x23 - AND $32,S
    // m.....m., 2 bytes, 5-m cycles
    instr!("AND @,S", AddressingMode::StackS, InstructionType::AND, 2),
    // 0x24 - BIT $10
    // mm....m., 2 bytes, 4-m+w cycles
    instr!("BIT @", AddressingMode::Direct, InstructionType::BIT, 2),
    // 0x25 - AND $10
    // m.....m., 2 bytes, 4-m+w cycles
    instr!("AND @", AddressingMode::Direct, InstructionType::AND, 2),
    // 0x26 - ROL $10
    // m.....mm, 2 bytes, 7-2*m+w cycles
    instr!("ROL @", AddressingMode::Direct, InstructionType::ROL, 2),
    // 0x27 - AND [$10]
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "AND [@]",
        AddressingMode::DirectPtr24,
        InstructionType::AND,
        2
    ),
    // 0x28 - PLP
    // ********, 1 bytes, 4 cycles
    instr!("PLP", AddressingMode::Implied, InstructionType::PLP, 1),
    // 0x29 - AND #$54
    // m.....m., 3-m bytes, 3-m cycles
    instr!(
        "AND #@",
        AddressingMode::ImmediateM,
        InstructionType::AND,
        3
    ),
    // 0x2A - ROL
    // m.....mm, 1 bytes, 2 cycles
    instr!("ROL", AddressingMode::Accumulator, InstructionType::ROL, 1),
    // 0x2B - PLD
    // *.....*., 1 bytes, 5 cycles
    instr!("PLD", AddressingMode::Implied, InstructionType::PLD, 1),
    // 0x2C - BIT $9876
    // mm....m., 3 bytes, 5-m cycles
    instr!("BIT @", AddressingMode::Absolute, InstructionType::BIT, 3),
    // 0x2D - AND $9876
    // m.....m., 3 bytes, 5-m cycles
    instr!("AND @", AddressingMode::Absolute, InstructionType::AND, 3),
    // 0x2E - ROL $9876
    // m.....mm, 3 bytes, 8-2*m cycles
    instr!("ROL @", AddressingMode::Absolute, InstructionType::ROL, 3),
    // 0x2F - AND $FEDBCA
    // m.....m., 4 bytes, 6-m cycles
    instr!("AND @", AddressingMode::Long, InstructionType::AND, 4),
    // 0x30 - BMI LABEL
    // ........, 2 bytes, 2+t+t*e*p cycles
    instr!("BMI @", AddressingMode::Relative8, InstructionType::BMI, 2),
    // 0x31 - AND ($10),Y
    // m.....m., 2 bytes, 7-m+w-x+x*p cycles
    instr!(
        "AND (@),Y",
        AddressingMode::DirectPtr16Y,
        InstructionType::AND,
        2
    ),
    // 0x32 - AND ($10)
    // m.....m., 2 bytes, 6-m+w cycles
    instr!(
        "AND (@)",
        AddressingMode::DirectPtr16,
        InstructionType::AND,
        2
    ),
    // 0x33 - AND ($32,S),Y
    // m.....m., 2 bytes, 8-m cycles
    instr!(
        "AND (@,S),Y",
        AddressingMode::StackSPtr16Y,
        InstructionType::AND,
        2
    ),
    // 0x34 - BIT $10,X
    // mm....m., 2 bytes, 5-m+w cycles
    instr!("BIT @,X", AddressingMode::DirectX, InstructionType::BIT, 2),
    // 0x35 - AND $10,X
    // m.....m., 2 bytes, 5-m+w cycles
    instr!("AND @,X", AddressingMode::DirectX, InstructionType::AND, 2),
    // 0x36 - ROL $10,X
    // m.....mm, 2 bytes, 8-2*m+w cycles
    instr!("ROL @,X", AddressingMode::DirectX, InstructionType::ROL, 2),
    // 0x37 - AND [$10],Y
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "AND [@],Y",
        AddressingMode::DirectPtr24Y,
        InstructionType::AND,
        2
    ),
    // 0x38 - SEC
    // .......1, 1 bytes, 2 cycles
    instr!("SEC", AddressingMode::Implied, InstructionType::SEC, 1),
    // 0x39 - AND $9876,Y
    // m.....m., 3 bytes, 6-m-x+x*p cycles
    instr!(
        "AND @,Y",
        AddressingMode::AbsoluteY,
        InstructionType::AND,
        3
    ),
    // 0x3A - DEC
    // m.....m., 1 bytes, 2 cycles
    instr!("DEC", AddressingMode::Accumulator, InstructionType::DEC, 1),
    // 0x3B - TSC
    // *.....*., 1 bytes, 2 cycles
    instr!("TSC", AddressingMode::Implied, InstructionType::TSC, 1),
    // 0x3C - BIT $9876,X
    // mm....m., 3 bytes, 6-m-x+x*p cycles
    instr!(
        "BIT @,X",
        AddressingMode::AbsoluteX,
        InstructionType::BIT,
        3
    ),
    // 0x3D - AND $9876,X
    // m.....m., 3 bytes, 6-m-x+x*p cycles
    instr!(
        "AND @,X",
        AddressingMode::AbsoluteX,
        InstructionType::AND,
        3
    ),
    // 0x3E - ROL $9876,X
    // m.....mm, 3 bytes, 9-2*m cycles
    instr!(
        "ROL @,X",
        AddressingMode::AbsoluteX,
        InstructionType::ROL,
        3
    ),
    // 0x3F - AND $FEDCBA,X
    // m.....m., 4 bytes, 6-m cycles
    instr!("AND @,X", AddressingMode::LongX, InstructionType::AND, 4),
    // 0x40 - RTI
    // ********, 1 bytes, 7-e cycles
    instr!("RTI", AddressingMode::Implied, InstructionType::RTI, 1),
    // 0x41 - EOR ($10,X)
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "EOR (@,X)",
        AddressingMode::DirectXPtr16,
        InstructionType::EOR,
        2
    ),
    // 0x42 - WDM
    // ........, 2 bytes, 2 cycles
    instr!("WDM", AddressingMode::Immediate8, InstructionType::WDM, 2),
    // 0x43 - EOR $32,S
    // m.....m., 2 bytes, 5-m cycles
    instr!("EOR @,S", AddressingMode::StackS, InstructionType::EOR, 2),
    // 0x44 - MVP #$12,#$34
    // ........, 3 bytes, 7 cycles
    instr!(
        "MVP #@,#@",
        AddressingMode::SrcDest,
        InstructionType::MVP,
        3
    ),
    // 0x45 - EOR $10
    // m.....m., 2 bytes, 4-m+w cycles
    instr!("EOR @", AddressingMode::Direct, InstructionType::EOR, 2),
    // 0x46 - LSR $10
    // 0.....m*, 2 bytes, 7-2*m+w cycles
    instr!("LSR @", AddressingMode::Direct, InstructionType::LSR, 2),
    // 0x47 - EOR [$10]
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "EOR [@]",
        AddressingMode::DirectPtr24,
        InstructionType::EOR,
        2
    ),
    // 0x48 - PHA
    // ........, 1 bytes, 4-m cycles
    instr!("PHA", AddressingMode::Implied, InstructionType::PHA, 1),
    // 0x49 - EOR #$54
    // m.....m., 3-m bytes, 3-m cycles
    instr!(
        "EOR #@",
        AddressingMode::ImmediateM,
        InstructionType::EOR,
        3
    ),
    // 0x4A - LSR
    // 0.....m*, 1 bytes, 2 cycles
    instr!("LSR", AddressingMode::Accumulator, InstructionType::LSR, 1),
    // 0x4B - PHK
    // ........, 1 bytes, 3 cycles
    instr!("PHK", AddressingMode::Implied, InstructionType::PHK, 1),
    // 0x4C - JMP $1234
    // ........, 3 bytes, 3 cycles
    instr!("JMP @", AddressingMode::Absolute, InstructionType::JMP, 3),
    // 0x4D - EOR $9876
    // m.....m., 3 bytes, 5-m cycles
    instr!("EOR @", AddressingMode::Absolute, InstructionType::EOR, 3),
    // 0x4E - LSR $9876
    // 0.....m*, 3 bytes, 8-2*m cycles
    instr!("LSR @", AddressingMode::Absolute, InstructionType::LSR, 3),
    // 0x4F - EOR $FEDBCA
    // m.....m., 4 bytes, 6-m cycles
    instr!("EOR @", AddressingMode::Long, InstructionType::EOR, 4),
    // 0x50 - BVC LABEL
    // ........, 2 bytes, 2+t+t*e*p cycles
    instr!("BVC @", AddressingMode::Relative8, InstructionType::BVC, 2),
    // 0x51 - EOR ($10),Y
    // m.....m., 2 bytes, 7-m+w-x+x*p cycles
    instr!(
        "EOR (@),Y",
        AddressingMode::DirectPtr16Y,
        InstructionType::EOR,
        2
    ),
    // 0x52 - EOR ($10)
    // m.....m., 2 bytes, 6-m+w cycles
    instr!(
        "EOR (@)",
        AddressingMode::DirectPtr16,
        InstructionType::EOR,
        2
    ),
    // 0x53 - EOR ($32,S),Y
    // m.....m., 2 bytes, 8-m cycles
    instr!(
        "EOR (@,S),Y",
        AddressingMode::StackSPtr16Y,
        InstructionType::EOR,
        2
    ),
    // 0x54 - MVN #$12,#$34
    // ........, 3 bytes, 7 cycles
    instr!(
        "MVN #@,#@",
        AddressingMode::SrcDest,
        InstructionType::MVN,
        3
    ),
    // 0x55 - EOR $10,X
    // m.....m., 2 bytes, 5-m+w cycles
    instr!("EOR @,X", AddressingMode::DirectX, InstructionType::EOR, 2),
    // 0x56 - LSR $10,X
    // 0.....m*, 2 bytes, 8-2*m+w cycles
    instr!("LSR @,X", AddressingMode::DirectX, InstructionType::LSR, 2),
    // 0x57 - EOR [$10],Y
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "EOR [@],Y",
        AddressingMode::DirectPtr24Y,
        InstructionType::EOR,
        2
    ),
    // 0x58 - CLI
    // .....0.., 1 bytes, 2 cycles
    instr!("CLI", AddressingMode::Implied, InstructionType::CLI, 1),
    // 0x59 - EOR $9876,Y
    // m.....m., 3 bytes, 6-m-x+x*p cycles
    instr!(
        "EOR @,Y",
        AddressingMode::AbsoluteY,
        InstructionType::EOR,
        3
    ),
    // 0x5A - PHY
    // ........, 1 bytes, 4-x cycles
    instr!("PHY", AddressingMode::Implied, InstructionType::PHY, 1),
    // 0x5B - TCD
    // *.....*., 1 bytes, 2 cycles
    instr!("TCD", AddressingMode::Implied, InstructionType::TCD, 1),
    // 0x5C - JMP $FEDCBA
    // ........, 4 bytes, 4 cycles
    instr!("JMP @", AddressingMode::Long, InstructionType::JMP, 4),
    // 0x5D - EOR $9876,X
    // m.....m., 3 bytes, 6-m-x+x*p cycles
    instr!(
        "EOR @,X",
        AddressingMode::AbsoluteX,
        InstructionType::EOR,
        3
    ),
    // 0x5E - LSR $9876,X
    // 0.....m*, 3 bytes, 9-2*m cycles
    instr!(
        "LSR @,X",
        AddressingMode::AbsoluteX,
        InstructionType::LSR,
        3
    ),
    // 0x5F - EOR $FEDCBA,X
    // m.....m., 4 bytes, 6-m cycles
    instr!("EOR @,X", AddressingMode::LongX, InstructionType::EOR, 4),
    // 0x60 - RTS
    // ........, 1 bytes, 6 cycles
    instr!("RTS", AddressingMode::Implied, InstructionType::RTS, 1),
    // 0x61 - ADC ($10,X)
    // mm....mm, 2 bytes, 7-m+w cycles
    instr!(
        "ADC (@,X)",
        AddressingMode::DirectXPtr16,
        InstructionType::ADC,
        2
    ),
    // 0x62 - PER LABEL
    // ........, 3 bytes, 6 cycles
    instr!("PER @", AddressingMode::Relative16, InstructionType::PER, 3),
    // 0x63 - ADC $32,S
    // mm....mm, 2 bytes, 5-m cycles
    instr!("ADC @,S", AddressingMode::StackS, InstructionType::ADC, 2),
    // 0x64 - STZ $10
    // ........, 2 bytes, 4-m+w cycles
    instr!("STZ @", AddressingMode::Direct, InstructionType::STZ, 2),
    // 0x65 - ADC $10
    // mm....mm, 2 bytes, 4-m+w cycles
    instr!("ADC @", AddressingMode::Direct, InstructionType::ADC, 2),
    // 0x66 - ROR $10
    // m.....m*, 2 bytes, 7-2*m+w cycles
    instr!("ROR @", AddressingMode::Direct, InstructionType::ROR, 2),
    // 0x67 - ADC [$10]
    // mm....mm, 2 bytes, 7-m+w cycles
    instr!(
        "ADC [@]",
        AddressingMode::DirectPtr24,
        InstructionType::ADC,
        2
    ),
    // 0x68 - PLA
    // m.....m., 1 bytes, 5-m cycles
    instr!("PLA", AddressingMode::Implied, InstructionType::PLA, 1),
    // 0x69 - ADC #$54
    // mm....mm, 3-m bytes, 3-m cycles
    instr!(
        "ADC #@",
        AddressingMode::ImmediateM,
        InstructionType::ADC,
        3
    ),
    // 0x6A - ROR
    // m.....m*, 1 bytes, 2 cycles
    instr!("ROR", AddressingMode::Accumulator, InstructionType::ROR, 1),
    // 0x6B - RTL
    // ........, 1 bytes, 6 cycles
    instr!("RTL", AddressingMode::Implied, InstructionType::RTL, 1),
    // 0x6C - JMP ($1234)
    // ........, 3 bytes, 5 cycles
    instr!(
        "JMP (@)",
        AddressingMode::AbsolutePtr16,
        InstructionType::JMP,
        3
    ),
    // 0x6D - ADC $9876
    // mm....mm, 3 bytes, 5-m cycles
    instr!("ADC @", AddressingMode::Absolute, InstructionType::ADC, 3),
    // 0x6E - ROR $9876
    // m.....m*, 3 bytes, 8-2*m cycles
    instr!("ROR @", AddressingMode::Absolute, InstructionType::ROR, 3),
    // 0x6F - ADC $FEDBCA
    // mm....mm, 4 bytes, 6-m cycles
    instr!("ADC @", AddressingMode::Long, InstructionType::ADC, 4),
    // 0x70 - BVS LABEL
    // ........, 2 bytes, 2+t+t*e*p cycles
    instr!("BVS @", AddressingMode::Relative8, InstructionType::BVS, 2),
    // 0x71 - ADC ($10),Y
    // mm....mm, 2 bytes, 7-m+w-x+x*p cycles
    instr!(
        "ADC (@),Y",
        AddressingMode::DirectPtr16Y,
        InstructionType::ADC,
        2
    ),
    // 0x72 - ADC ($10)
    // mm....mm, 2 bytes, 6-m+w cycles
    instr!(
        "ADC (@)",
        AddressingMode::DirectPtr16,
        InstructionType::ADC,
        2
    ),
    // 0x73 - ADC ($32,S),Y
    // mm....mm, 2 bytes, 8-m cycles
    instr!(
        "ADC (@,S),Y",
        AddressingMode::StackSPtr16Y,
        InstructionType::ADC,
        2
    ),
    // 0x74 - STZ $10,X
    // ........, 2 bytes, 5-m+w cycles
    instr!("STZ @,X", AddressingMode::DirectX, InstructionType::STZ, 2),
    // 0x75 - ADC $10,X
    // mm....mm, 2 bytes, 5-m+w cycles
    instr!("ADC @,X", AddressingMode::DirectX, InstructionType::ADC, 2),
    // 0x76 - ROR $10,X
    // m.....m*, 2 bytes, 8-2*m+w cycles
    instr!("ROR @,X", AddressingMode::DirectX, InstructionType::ROR, 2),
    // 0x77 - ADC [$10],Y
    // mm....mm, 2 bytes, 7-m+w cycles
    instr!(
        "ADC [@],Y",
        AddressingMode::DirectPtr24Y,
        InstructionType::ADC,
        2
    ),
    // 0x78 - SEI
    // .....1.., 1 bytes, 2 cycles
    instr!("SEI", AddressingMode::Implied, InstructionType::SEI, 1),
    // 0x79 - ADC $9876,Y
    // mm....mm, 3 bytes, 6-m-x+x*p cycles
    instr!(
        "ADC @,Y",
        AddressingMode::AbsoluteY,
        InstructionType::ADC,
        3
    ),
    // 0x7A - PLY
    // x.....x., 1 bytes, 5-x cycles
    instr!("PLY", AddressingMode::Implied, InstructionType::PLY, 1),
    // 0x7B - TDC
    // *.....*., 1 bytes, 2 cycles
    instr!("TDC", AddressingMode::Implied, InstructionType::TDC, 1),
    // 0x7C - JMP ($1234,X)
    // ........, 3 bytes, 6 cycles
    instr!(
        "JMP (@,X)",
        AddressingMode::AbsoluteXPtr16,
        InstructionType::JMP,
        3
    ),
    // 0x7D - ADC $9876,X
    // mm....mm, 3 bytes, 6-m-x+x*p cycles
    instr!(
        "ADC @,X",
        AddressingMode::AbsoluteX,
        InstructionType::ADC,
        3
    ),
    // 0x7E - ROR $9876,X
    // m.....m*, 3 bytes, 9-2*m cycles
    instr!(
        "ROR @,X",
        AddressingMode::AbsoluteX,
        InstructionType::ROR,
        3
    ),
    // 0x7F - ADC $FEDCBA,X
    // mm....mm, 4 bytes, 6-m cycles
    instr!("ADC @,X", AddressingMode::LongX, InstructionType::ADC, 4),
    // 0x80 - BRA LABEL
    // ........, 2 bytes, 3+e*p cycles
    instr!("BRA @", AddressingMode::Relative8, InstructionType::BRA, 2),
    // 0x81 - STA ($10,X)
    // ........, 2 bytes, 7-m+w cycles
    instr!(
        "STA (@,X)",
        AddressingMode::DirectXPtr16,
        InstructionType::STA,
        2
    ),
    // 0x82 - BRL LABEL
    // ........, 3 bytes, 4 cycles
    instr!("BRL @", AddressingMode::Relative16, InstructionType::BRL, 3),
    // 0x83 - STA $32,S
    // ........, 2 bytes, 5-m cycles
    instr!("STA @,S", AddressingMode::StackS, InstructionType::STA, 2),
    // 0x84 - STY $10
    // ........, 2 bytes, 4-x+w cycles
    instr!("STY @", AddressingMode::Direct, InstructionType::STY, 2),
    // 0x85 - STA $10
    // ........, 2 bytes, 4-m+w cycles
    instr!("STA @", AddressingMode::Direct, InstructionType::STA, 2),
    // 0x86 - STX $10
    // ........, 2 bytes, 4-x+w cycles
    instr!("STX @", AddressingMode::Direct, InstructionType::STX, 2),
    // 0x87 - STA [$10]
    // ........, 2 bytes, 7-m+w cycles
    instr!(
        "STA [@]",
        AddressingMode::DirectPtr24,
        InstructionType::STA,
        2
    ),
    // 0x88 - DEY
    // x.....x., 1 bytes, 2 cycles
    instr!("DEY", AddressingMode::Implied, InstructionType::DEY, 1),
    // 0x89 - BIT #$54
    // ......m., 3-m bytes, 3-m cycles
    instr!(
        "BIT #@",
        AddressingMode::ImmediateM,
        InstructionType::BIT,
        3
    ),
    // 0x8A - TXA
    // m.....m., 1 bytes, 2 cycles
    instr!("TXA", AddressingMode::Implied, InstructionType::TXA, 1),
    // 0x8B - PHB
    // ........, 1 bytes, 3 cycles
    instr!("PHB", AddressingMode::Implied, InstructionType::PHB, 1),
    // 0x8C - STY $9876
    // ........, 3 bytes, 5-x cycles
    instr!("STY @", AddressingMode::Absolute, InstructionType::STY, 3),
    // 0x8D - STA $9876
    // ........, 3 bytes, 5-m cycles
    instr!("STA @", AddressingMode::Absolute, InstructionType::STA, 3),
    // 0x8E - STX $9876
    // ........, 3 bytes, 5-x cycles
    instr!("STX @", AddressingMode::Absolute, InstructionType::STX, 3),
    // 0x8F - STA $FEDBCA
    // ........, 4 bytes, 6-m cycles
    instr!("STA @", AddressingMode::Long, InstructionType::STA, 4),
    // 0x90 - BCC LABEL
    // ........, 2 bytes, 2+t+t*e*p cycles
    instr!("BCC @", AddressingMode::Relative8, InstructionType::BCC, 2),
    // 0x91 - STA ($10),Y
    // ........, 2 bytes, 7-m+w cycles
    instr!(
        "STA (@),Y",
        AddressingMode::DirectPtr16Y,
        InstructionType::STA,
        2
    ),
    // 0x92 - STA ($10)
    // ........, 2 bytes, 6-m+w cycles
    instr!(
        "STA (@)",
        AddressingMode::DirectPtr16,
        InstructionType::STA,
        2
    ),
    // 0x93 - STA ($32,S),Y
    // ........, 2 bytes, 8-m cycles
    instr!(
        "STA (@,S),Y",
        AddressingMode::StackSPtr16Y,
        InstructionType::STA,
        2
    ),
    // 0x94 - STY $10,X
    // ........, 2 bytes, 5-x+w cycles
    instr!("STY @,X", AddressingMode::DirectX, InstructionType::STY, 2),
    // 0x95 - STA $10,X
    // ........, 2 bytes, 5-m+w cycles
    instr!("STA @,X", AddressingMode::DirectX, InstructionType::STA, 2),
    // 0x96 - STX $10,Y
    // ........, 2 bytes, 5-x+w cycles
    instr!("STX @,Y", AddressingMode::DirectY, InstructionType::STX, 2),
    // 0x97 - STA [$10],Y
    // ........, 2 bytes, 7-m+w cycles
    instr!(
        "STA [@],Y",
        AddressingMode::DirectPtr24Y,
        InstructionType::STA,
        2
    ),
    // 0x98 - TYA
    // m.....m., 1 bytes, 2 cycles
    instr!("TYA", AddressingMode::Implied, InstructionType::TYA, 1),
    // 0x99 - STA $9876,Y
    // ........, 3 bytes, 6-m cycles
    instr!(
        "STA @,Y",
        AddressingMode::AbsoluteY,
        InstructionType::STA,
        3
    ),
    // 0x9A - TXS
    // ........, 1 bytes, 2 cycles
    instr!("TXS", AddressingMode::Implied, InstructionType::TXS, 1),
    // 0x9B - TXY
    // x.....x., 1 bytes, 2 cycles
    instr!("TXY", AddressingMode::Implied, InstructionType::TXY, 1),
    // 0x9C - STZ $9876
    // ........, 3 bytes, 5-m cycles
    instr!("STZ @", AddressingMode::Absolute, InstructionType::STZ, 3),
    // 0x9D - STA $9876,X
    // ........, 3 bytes, 6-m cycles
    instr!(
        "STA @,X",
        AddressingMode::AbsoluteX,
        InstructionType::STA,
        3
    ),
    // 0x9E - STZ $9876,X
    // ........, 3 bytes, 6-m cycles
    instr!(
        "STZ @,X",
        AddressingMode::AbsoluteX,
        InstructionType::STZ,
        3
    ),
    // 0x9F - STA $FEDCBA,X
    // ........, 4 bytes, 6-m cycles
    instr!("STA @,X", AddressingMode::LongX, InstructionType::STA, 4),
    // 0xA0 - LDY #$54
    // x.....x., 3-x bytes, 3-x cycles
    instr!(
        "LDY #@",
        AddressingMode::ImmediateX,
        InstructionType::LDY,
        3
    ),
    // 0xA1 - LDA ($10,X)
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "LDA (@,X)",
        AddressingMode::DirectXPtr16,
        InstructionType::LDA,
        2
    ),
    // 0xA2 - LDX #$54
    // x.....x., 3-x bytes, 3-x cycles
    instr!(
        "LDX #@",
        AddressingMode::ImmediateX,
        InstructionType::LDX,
        3
    ),
    // 0xA3 - LDA $32,S
    // m.....m., 2 bytes, 5-m cycles
    instr!("LDA @,S", AddressingMode::StackS, InstructionType::LDA, 2),
    // 0xA4 - LDY $10
    // x.....x., 2 bytes, 4-x+w cycles
    instr!("LDY @", AddressingMode::Direct, InstructionType::LDY, 2),
    // 0xA5 - LDA $10
    // m.....m., 2 bytes, 4-m+w cycles
    instr!("LDA @", AddressingMode::Direct, InstructionType::LDA, 2),
    // 0xA6 - LDX $10
    // x.....x., 2 bytes, 4-x+w cycles
    instr!("LDX @", AddressingMode::Direct, InstructionType::LDX, 2),
    // 0xA7 - LDA [$10]
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "LDA [@]",
        AddressingMode::DirectPtr24,
        InstructionType::LDA,
        2
    ),
    // 0xA8 - TAY
    // x.....x., 1 bytes, 2 cycles
    instr!("TAY", AddressingMode::Implied, InstructionType::TAY, 1),
    // 0xA9 - LDA #$54
    // m.....m., 3-m bytes, 3-m cycles
    instr!(
        "LDA #@",
        AddressingMode::ImmediateM,
        InstructionType::LDA,
        3
    ),
    // 0xAA - TAX
    // x.....x., 1 bytes, 2 cycles
    instr!("TAX", AddressingMode::Implied, InstructionType::TAX, 1),
    // 0xAB - PLB
    // *.....*., 1 bytes, 4 cycles
    instr!("PLB", AddressingMode::Implied, InstructionType::PLB, 1),
    // 0xAC - LDY $9876
    // x.....x., 3 bytes, 5-x cycles
    instr!("LDY @", AddressingMode::Absolute, InstructionType::LDY, 3),
    // 0xAD - LDA $9876
    // m.....m., 3 bytes, 5-m cycles
    instr!("LDA @", AddressingMode::Absolute, InstructionType::LDA, 3),
    // 0xAE - LDX $9876
    // x.....x., 3 bytes, 5-x cycles
    instr!("LDX @", AddressingMode::Absolute, InstructionType::LDX, 3),
    // 0xAF - LDA $FEDBCA
    // m.....m., 4 bytes, 6-m cycles
    instr!("LDA @", AddressingMode::Long, InstructionType::LDA, 4),
    // 0xB0 - BCS LABEL
    // ........, 2 bytes, 2+t+t*e*p cycles
    instr!("BCS @", AddressingMode::Relative8, InstructionType::BCS, 2),
    // 0xB1 - LDA ($10),Y
    // m.....m., 2 bytes, 7-m+w-x+x*p cycles
    instr!(
        "LDA (@),Y",
        AddressingMode::DirectPtr16Y,
        InstructionType::LDA,
        2
    ),
    // 0xB2 - LDA ($10)
    // m.....m., 2 bytes, 6-m+w cycles
    instr!(
        "LDA (@)",
        AddressingMode::DirectPtr16,
        InstructionType::LDA,
        2
    ),
    // 0xB3 - LDA ($32,S),Y
    // m.....m., 2 bytes, 8-m cycles
    instr!(
        "LDA (@,S),Y",
        AddressingMode::StackSPtr16Y,
        InstructionType::LDA,
        2
    ),
    // 0xB4 - LDY $10,X
    // x.....x., 2 bytes, 5-x+w cycles
    instr!("LDY @,X", AddressingMode::DirectX, InstructionType::LDY, 2),
    // 0xB5 - LDA $10,X
    // m.....m., 2 bytes, 5-m+w cycles
    instr!("LDA @,X", AddressingMode::DirectX, InstructionType::LDA, 2),
    // 0xB6 - LDX $10,Y
    // x.....x., 2 bytes, 5-x+w cycles
    instr!("LDX @,Y", AddressingMode::DirectY, InstructionType::LDX, 2),
    // 0xB7 - LDA [$10],Y
    // m.....m., 2 bytes, 7-m+w cycles
    instr!(
        "LDA [@],Y",
        AddressingMode::DirectPtr24Y,
        InstructionType::LDA,
        2
    ),
    // 0xB8 - CLV
    // .0......, 1 bytes, 2 cycles
    instr!("CLV", AddressingMode::Implied, InstructionType::CLV, 1),
    // 0xB9 - LDA $9876,Y
    // m.....m., 3 bytes, 6-m-x+x*p cycles
    instr!(
        "LDA @,Y",
        AddressingMode::AbsoluteY,
        InstructionType::LDA,
        3
    ),
    // 0xBA - TSX
    // x.....x., 1 bytes, 2 cycles
    instr!("TSX", AddressingMode::Implied, InstructionType::TSX, 1),
    // 0xBB - TYX
    // x.....x., 1 bytes, 2 cycles
    instr!("TYX", AddressingMode::Implied, InstructionType::TYX, 1),
    // 0xBC - LDY $9876,X
    // x.....x., 3 bytes, 6-2*x+x*p cycles
    instr!(
        "LDY @,X",
        AddressingMode::AbsoluteX,
        InstructionType::LDY,
        3
    ),
    // 0xBD - LDA $9876,X
    // m.....m., 3 bytes, 6-m-x+x*p cycles
    instr!(
        "LDA @,X",
        AddressingMode::AbsoluteX,
        InstructionType::LDA,
        3
    ),
    // 0xBE - LDX $9876,Y
    // x.....x., 3 bytes, 6-2*x+x*p cycles
    instr!(
        "LDX @,Y",
        AddressingMode::AbsoluteY,
        InstructionType::LDX,
        3
    ),
    // 0xBF - LDA $FEDCBA,X
    // m.....m., 4 bytes, 6-m cycles
    instr!("LDA @,X", AddressingMode::LongX, InstructionType::LDA, 4),
    // 0xC0 - CPY #$54
    // x.....xx, 3-x bytes, 3-x cycles
    instr!(
        "CPY #@",
        AddressingMode::ImmediateX,
        InstructionType::CPY,
        3
    ),
    // 0xC1 - CMP ($10,X)
    // m.....mm, 2 bytes, 7-m+w cycles
    instr!(
        "CMP (@,X)",
        AddressingMode::DirectXPtr16,
        InstructionType::CMP,
        2
    ),
    // 0xC2 - REP #$12
    // ********, 2 bytes, 3 cycles
    instr!(
        "REP #@",
        AddressingMode::Immediate8,
        InstructionType::REP,
        2
    ),
    // 0xC3 - CMP $32,S
    // m.....mm, 2 bytes, 5-m cycles
    instr!("CMP @,S", AddressingMode::StackS, InstructionType::CMP, 2),
    // 0xC4 - CPY $10
    // x.....xx, 2 bytes, 4-x+w cycles
    instr!("CPY @", AddressingMode::Direct, InstructionType::CPY, 2),
    // 0xC5 - CMP $10
    // m.....mm, 2 bytes, 4-m+w cycles
    instr!("CMP @", AddressingMode::Direct, InstructionType::CMP, 2),
    // 0xC6 - DEC $10
    // m.....m., 2 bytes, 7-2*m+w cycles
    instr!("DEC @", AddressingMode::Direct, InstructionType::DEC, 2),
    // 0xC7 - CMP [$10]
    // m.....mm, 2 bytes, 7-m+w cycles
    instr!(
        "CMP [@]",
        AddressingMode::DirectPtr24,
        InstructionType::CMP,
        2
    ),
    // 0xC8 - INY
    // x.....x., 1 bytes, 2 cycles
    instr!("INY", AddressingMode::Implied, InstructionType::INY, 1),
    // 0xC9 - CMP #$54
    // m.....mm, 3-m bytes, 3-m cycles
    instr!(
        "CMP #@",
        AddressingMode::ImmediateM,
        InstructionType::CMP,
        3
    ),
    // 0xCA - DEX
    // x.....x., 1 bytes, 2 cycles
    instr!("DEX", AddressingMode::Implied, InstructionType::DEX, 1),
    // 0xCB - WAI
    // ........, 1 bytes, 3 cycles
    instr!("WAI", AddressingMode::Implied, InstructionType::WAI, 1),
    // 0xCC - CPY $9876
    // x.....xx, 3 bytes, 5-x cycles
    instr!("CPY @", AddressingMode::Absolute, InstructionType::CPY, 3),
    // 0xCD - CMP $9876
    // m.....mm, 3 bytes, 5-m cycles
    instr!("CMP @", AddressingMode::Absolute, InstructionType::CMP, 3),
    // 0xCE - DEC $9876
    // m.....m., 3 bytes, 8-2*m cycles
    instr!("DEC @", AddressingMode::Absolute, InstructionType::DEC, 3),
    // 0xCF - CMP $FEDBCA
    // m.....mm, 4 bytes, 6-m cycles
    instr!("CMP @", AddressingMode::Long, InstructionType::CMP, 4),
    // 0xD0 - BNE LABEL
    // ........, 2 bytes, 2+t+t*e*p cycles
    instr!("BNE @", AddressingMode::Relative8, InstructionType::BNE, 2),
    // 0xD1 - CMP ($10),Y
    // m.....mm, 2 bytes, 7-m+w-x+x*p cycles
    instr!(
        "CMP (@),Y",
        AddressingMode::DirectPtr16Y,
        InstructionType::CMP,
        2
    ),
    // 0xD2 - CMP ($10)
    // m.....mm, 2 bytes, 6-m+w cycles
    instr!(
        "CMP (@)",
        AddressingMode::DirectPtr16,
        InstructionType::CMP,
        2
    ),
    // 0xD3 - CMP ($32,S),Y
    // m.....mm, 2 bytes, 8-m cycles
    instr!(
        "CMP (@,S),Y",
        AddressingMode::StackSPtr16Y,
        InstructionType::CMP,
        2
    ),
    // 0xD4 - PEI $12
    // ........, 2 bytes, 6+w cycles
    instr!("PEI @", AddressingMode::Direct, InstructionType::PEI, 2),
    // 0xD5 - CMP $10,X
    // m.....mm, 2 bytes, 5-m+w cycles
    instr!("CMP @,X", AddressingMode::DirectX, InstructionType::CMP, 2),
    // 0xD6 - DEC $10,X
    // m.....m., 2 bytes, 8-2*m+w cycles
    instr!("DEC @,X", AddressingMode::DirectX, InstructionType::DEC, 2),
    // 0xD7 - CMP [$10],Y
    // m.....mm, 2 bytes, 7-m+w cycles
    instr!(
        "CMP [@],Y",
        AddressingMode::DirectPtr24Y,
        InstructionType::CMP,
        2
    ),
    // 0xD8 - CLD
    // ....0..., 1 bytes, 2 cycles
    instr!("CLD", AddressingMode::Implied, InstructionType::CLD, 1),
    // 0xD9 - CMP $9876,Y
    // m.....mm, 3 bytes, 6-m-x+x*p cycles
    instr!(
        "CMP @,Y",
        AddressingMode::AbsoluteY,
        InstructionType::CMP,
        3
    ),
    // 0xDA - PHX
    // ........, 1 bytes, 4-x cycles
    instr!("PHX", AddressingMode::Implied, InstructionType::PHX, 1),
    // 0xDB - STP
    // ........, 1 bytes, 3 cycles
    instr!("STP", AddressingMode::Implied, InstructionType::STP, 1),
    // 0xDC - JMP [$1234]
    // ........, 3 bytes, 6 cycles
    instr!(
        "JMP [@]",
        AddressingMode::AbsolutePtr24,
        InstructionType::JMP,
        3
    ),
    // 0xDD - CMP $9876,X
    // m.....mm, 3 bytes, 6-m-x+x*p cycles
    instr!(
        "CMP @,X",
        AddressingMode::AbsoluteX,
        InstructionType::CMP,
        3
    ),
    // 0xDE - DEC $9876,X
    // m.....m., 3 bytes, 9-2*m cycles
    instr!(
        "DEC @,X",
        AddressingMode::AbsoluteX,
        InstructionType::DEC,
        3
    ),
    // 0xDF - CMP $FEDCBA,X
    // m.....mm, 4 bytes, 6-m cycles
    instr!("CMP @,X", AddressingMode::LongX, InstructionType::CMP, 4),
    // 0xE0 - CPX #$54
    // x.....xx, 3-x bytes, 3-x cycles
    instr!(
        "CPX #@",
        AddressingMode::ImmediateX,
        InstructionType::CPX,
        3
    ),
    // 0xE1 - SBC ($10,X)
    // mm....mm, 2 bytes, 7-m+w cycles
    instr!(
        "SBC (@,X)",
        AddressingMode::DirectXPtr16,
        InstructionType::SBC,
        2
    ),
    // 0xE2 - SEP #$12
    // ********, 2 bytes, 3 cycles
    instr!(
        "SEP #@",
        AddressingMode::Immediate8,
        InstructionType::SEP,
        2
    ),
    // 0xE3 - SBC $32,S
    // mm....mm, 2 bytes, 5-m cycles
    instr!("SBC @,S", AddressingMode::StackS, InstructionType::SBC, 2),
    // 0xE4 - CPX $10
    // x.....xx, 2 bytes, 4-x+w cycles
    instr!("CPX @", AddressingMode::Direct, InstructionType::CPX, 2),
    // 0xE5 - SBC $10
    // mm....mm, 2 bytes, 4-m+w cycles
    instr!("SBC @", AddressingMode::Direct, InstructionType::SBC, 2),
    // 0xE6 - INC $10
    // m.....m., 2 bytes, 7-2*m+w cycles
    instr!("INC @", AddressingMode::Direct, InstructionType::INC, 2),
    // 0xE7 - SBC [$10]
    // mm....mm, 2 bytes, 7-m+w cycles
    instr!(
        "SBC [@]",
        AddressingMode::DirectPtr24,
        InstructionType::SBC,
        2
    ),
    // 0xE8 - INX
    // x.....x., 1 bytes, 2 cycles
    instr!("INX", AddressingMode::Implied, InstructionType::INX, 1),
    // 0xE9 - SBC #$54
    // mm....mm, 3-m bytes, 3-m cycles
    instr!(
        "SBC #@",
        AddressingMode::ImmediateM,
        InstructionType::SBC,
        3
    ),
    // 0xEA - NOP
    // ........, 1 bytes, 2 cycles
    instr!("NOP", AddressingMode::Implied, InstructionType::NOP, 1),
    // 0xEB - XBA
    // *.....*., 1 bytes, 3 cycles
    instr!("XBA", AddressingMode::Implied, InstructionType::XBA, 1),
    // 0xEC - CPX $9876
    // x.....xx, 3 bytes, 5-x cycles
    instr!("CPX @", AddressingMode::Absolute, InstructionType::CPX, 3),
    // 0xED - SBC $9876
    // mm....mm, 3 bytes, 5-m cycles
    instr!("SBC @", AddressingMode::Absolute, InstructionType::SBC, 3),
    // 0xEE - INC $9876
    // m.....m., 3 bytes, 8-2*m cycles
    instr!("INC @", AddressingMode::Absolute, InstructionType::INC, 3),
    // 0xEF - SBC $FEDBCA
    // mm....mm, 4 bytes, 6-m cycles
    instr!("SBC @", AddressingMode::Long, InstructionType::SBC, 4),
    // 0xF0 - BEQ LABEL
    // ........, 2 bytes, 2+t+t*e*p cycles
    instr!("BEQ @", AddressingMode::Relative8, InstructionType::BEQ, 2),
    // 0xF1 - SBC ($10),Y
    // mm....mm, 2 bytes, 7-m+w-x+x*p cycles
    instr!(
        "SBC (@),Y",
        AddressingMode::DirectPtr16Y,
        InstructionType::SBC,
        2
    ),
    // 0xF2 - SBC ($10)
    // mm....mm, 2 bytes, 6-m+w cycles
    instr!(
        "SBC (@)",
        AddressingMode::DirectPtr16,
        InstructionType::SBC,
        2
    ),
    // 0xF3 - SBC ($32,S),Y
    // mm....mm, 2 bytes, 8-m cycles
    instr!(
        "SBC (@,S),Y",
        AddressingMode::StackSPtr16Y,
        InstructionType::SBC,
        2
    ),
    // 0xF4 - PEA #$1234
    // ........, 3 bytes, 5 cycles
    instr!(
        "PEA #@",
        AddressingMode::Immediate16,
        InstructionType::PEA,
        3
    ),
    // 0xF5 - SBC $10,X
    // mm....mm, 2 bytes, 5-m+w cycles
    instr!("SBC @,X", AddressingMode::DirectX, InstructionType::SBC, 2),
    // 0xF6 - INC $10,X
    // m.....m., 2 bytes, 8-2*m+w cycles
    instr!("INC @,X", AddressingMode::DirectX, InstructionType::INC, 2),
    // 0xF7 - SBC [$10],Y
    // mm....mm, 2 bytes, 7-m+w cycles
    instr!(
        "SBC [@],Y",
        AddressingMode::DirectPtr24Y,
        InstructionType::SBC,
        2
    ),
    // 0xF8 - SED
    // ....1..., 1 bytes, 2 cycles
    instr!("SED", AddressingMode::Implied, InstructionType::SED, 1),
    // 0xF9 - SBC $9876,Y
    // mm....mm, 3 bytes, 6-m-x+x*p cycles
    instr!(
        "SBC @,Y",
        AddressingMode::AbsoluteY,
        InstructionType::SBC,
        3
    ),
    // 0xFA - PLX
    // x.....x., 1 bytes, 5-x cycles
    instr!("PLX", AddressingMode::Implied, InstructionType::PLX, 1),
    // 0xFB - XCE
    // .......*, 1 bytes, 2 cycles
    instr!("XCE", AddressingMode::Implied, InstructionType::XCE, 1),
    // 0xFC - JSR ($1234,X)
    // ........, 3 bytes, 8 cycles
    instr!(
        "JSR (@,X)",
        AddressingMode::AbsoluteXPtr16,
        InstructionType::JSR,
        3
    ),
    // 0xFD - SBC $9876,X
    // mm....mm, 3 bytes, 6-m-x+x*p cycles
    instr!(
        "SBC @,X",
        AddressingMode::AbsoluteX,
        InstructionType::SBC,
        3
    ),
    // 0xFE - INC $9876,X
    // m.....m., 3 bytes, 9-2*m cycles
    instr!(
        "INC @,X",
        AddressingMode::AbsoluteX,
        InstructionType::INC,
        3
    ),
    // 0xFF - SBC $FEDCBA,X
    // mm....mm, 4 bytes, 6-m cycles
    instr!("SBC @,X", AddressingMode::LongX, InstructionType::SBC, 4),
];
