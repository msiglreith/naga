/*! SPIR-V frontend

## ID lookups

Our IR links to everything with `Token`, while SPIR-V uses IDs.
In order to keep track of the associations, the parser has many lookup tables.
There map `spirv::Word` into a specific IR token, plus potentially a bit of
extra info, such as the related SPIR-V type ID.
TODO: would be nice to find ways that avoid looking up as much

!*/

use crate::{
    storage::{Storage, Token},
    FastHashMap, FastHashSet,
};

use std::convert::TryInto;

const LAST_KNOWN_OPCODE: spirv::Op = spirv::Op::MemberDecorateStringGOOGLE;
const LAST_KNOWN_CAPABILITY: spirv::Capability = spirv::Capability::VulkanMemoryModelDeviceScopeKHR;
const LAST_KNOWN_EXECUTION_MODEL: spirv::ExecutionModel = spirv::ExecutionModel::Kernel;
const LAST_KNOWN_STORAGE_CLASS: spirv::StorageClass = spirv::StorageClass::StorageBuffer;
const LAST_KNOWN_DECORATION: spirv::Decoration = spirv::Decoration::NonUniformEXT;
const LAST_KNOWN_BUILT_IN: spirv::BuiltIn = spirv::BuiltIn::FullyCoveredEXT;
const LAST_KNOWN_DIM: spirv::Dim = spirv::Dim::DimSubpassData;

pub const SUPPORTED_CAPABILITIES: &[spirv::Capability] = &[
    spirv::Capability::Shader,
];
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
];
pub const SUPPORTED_EXT_SETS: &[&str] = &[
    "GLSL.std.450",
];

#[derive(Debug)]
pub enum Error {
    InvalidHeader,
    InvalidWordCount,
    UnknownInstruction(u16),
    UnknownCapability(u32),
    UnsupportedInstruction(ModuleState, spirv::Op),
    UnsupportedCapability(spirv::Capability),
    UnsupportedExtension(String),
    UnsupportedExtSet(String),
    UnsupportedType(Token<crate::Type>),
    UnsupportedExecutionModel(u32),
    UnsupportedStorageClass(u32),
    UnsupportedFunctionControl(u32),
    UnsupportedDim(u32),
    InvalidParameter(spirv::Op),
    InvalidOperandCount(spirv::Op, u16),
    InvalidOperand,
    InvalidDecoration(spirv::Word),
    InvalidId(spirv::Word),
    InvalidTypeWidth(spirv::Word),
    InvalidSign(spirv::Word),
    InvalidInnerType(spirv::Word),
    InvalidVectorSize(spirv::Word),
    InvalidVariableClass(spirv::StorageClass),
    InvalidAccessType(spirv::Word),
    InvalidAccessIndex(Token<crate::Expression>),
    InvalidLoadType(spirv::Word),
    InvalidStoreType(spirv::Word),
    InvalidBinding(spirv::Word),
    WrongFunctionResultType(spirv::Word),
    WrongFunctionParameterType(spirv::Word),
    BadString,
    IncompleteData,
}

struct Instruction {
    op: spirv::Op,
    wc: u16,
}

impl Instruction {
    fn expect(&self, count: u16) -> Result<(), Error> {
        if self.wc == count {
            Ok(())
        } else {
            Err(Error::InvalidOperandCount(self.op, self.wc))
        }
    }

    fn expect_at_least(&self, count: u16) -> Result<(), Error> {
        if self.wc >= count {
            Ok(())
        } else {
            Err(Error::InvalidOperandCount(self.op, self.wc))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub enum ModuleState {
    Empty,
    Capability,
    Extension,
    ExtInstImport,
    MemoryModel,
    EntryPoint,
    ExecutionMode,
    Source,
    Name,
    ModuleProcessed,
    Annotation,
    Type,
    Function,
}

trait LookupHelper {
    type Target;
    fn lookup(&self, key: spirv::Word) -> Result<&Self::Target, Error>;
}

impl<T> LookupHelper for FastHashMap<spirv::Word, T> {
    type Target = T;
    fn lookup(&self, key: spirv::Word) -> Result<&T, Error> {
        self.get(&key)
            .ok_or(Error::InvalidId(key))
    }
}

fn map_vector_size(word: spirv::Word) -> Result<crate::VectorSize, Error> {
    match word {
        2 => Ok(crate::VectorSize::Bi),
        3 => Ok(crate::VectorSize::Tri),
        4 => Ok(crate::VectorSize::Quad),
        _ => Err(Error::InvalidVectorSize(word))
    }
}

fn map_storage_class(word: spirv::Word) -> Result<spirv::StorageClass, Error> {
    if word > LAST_KNOWN_STORAGE_CLASS as u32 {
        Err(Error::UnsupportedStorageClass(word))
    } else {
        Ok(unsafe { std::mem::transmute(word) })
    }
}

type MemberIndex = u32;

#[derive(Debug, Default)]
struct Decoration {
    name: Option<String>,
    built_in: Option<spirv::BuiltIn>,
    location: Option<spirv::Word>,
    desc_set: Option<spirv::Word>,
    desc_index: Option<spirv::Word>,
}

impl Decoration {
    fn get_binding(&self) -> Option<crate::Binding> {
        //TODO: validate this better
        match *self {
            Decoration {
                built_in: Some(built_in),
                location: None,
                desc_set: None,
                desc_index: None,
                ..
            } => Some(crate::Binding::BuiltIn(built_in)),
            Decoration {
                built_in: None,
                location: Some(loc),
                desc_set: None,
                desc_index: None,
                ..
            } => Some(crate::Binding::Location(loc)),
            Decoration {
                built_in: None,
                location: None,
                desc_set: Some(set),
                desc_index: Some(binding),
                ..
            } => Some(crate::Binding::Descriptor { set, binding }),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct LookupFunctionType {
    parameter_type_ids: Vec<spirv::Word>,
    return_type_id: spirv::Word,
}

#[derive(Debug)]
struct EntryPoint {
    exec_model: spirv::ExecutionModel,
    name: String,
    function_id: spirv::Word,
    variable_ids: Vec<spirv::Word>,
}

#[derive(Debug)]
struct LookupType {
    token: Token<crate::Type>,
    base_id: Option<spirv::Word>,
}

#[derive(Debug)]
struct LookupConstant {
    token: Token<crate::Constant>,
    type_id: spirv::Word,
}

#[derive(Debug)]
struct LookupVariable {
    token: Token<crate::GlobalVariable>,
    type_id: spirv::Word,
}

#[derive(Clone, Debug)]
struct LookupExpression {
    token: Token<crate::Expression>,
    type_id: spirv::Word,
}

#[derive(Clone, Debug)]
struct LookupSampledImage {
    image: Token<crate::Expression>,
    sampler: Token<crate::Expression>,
}

pub struct Parser<I> {
    data: I,
    state: ModuleState,
    temp_bytes: Vec<u8>,
    future_decor: FastHashMap<spirv::Word, Decoration>,
    future_member_decor: FastHashMap<(spirv::Word, MemberIndex), Decoration>,
    lookup_member_type_id: FastHashMap<(spirv::Word, MemberIndex), spirv::Word>,
    lookup_type: FastHashMap<spirv::Word, LookupType>,
    lookup_void_type: FastHashSet<spirv::Word>,
    lookup_constant: FastHashMap<spirv::Word, LookupConstant>,
    lookup_variable: FastHashMap<spirv::Word, LookupVariable>,
    lookup_expression: FastHashMap<spirv::Word, LookupExpression>,
    lookup_sampled_image: FastHashMap<spirv::Word, LookupSampledImage>,
    lookup_function_type: FastHashMap<spirv::Word, LookupFunctionType>,
    lookup_function: FastHashMap<spirv::Word, Token<crate::Function>>,
}

impl<I: Iterator<Item = u32>> Parser<I> {
    pub fn new(data: I) -> Self {
        Parser {
            data,
            state: ModuleState::Empty,
            temp_bytes: Vec::new(),
            future_decor: FastHashMap::default(),
            future_member_decor: FastHashMap::default(),
            lookup_member_type_id: FastHashMap::default(),
            lookup_type: FastHashMap::default(),
            lookup_void_type: FastHashSet::default(),
            lookup_constant: FastHashMap::default(),
            lookup_variable: FastHashMap::default(),
            lookup_expression: FastHashMap::default(),
            lookup_sampled_image: FastHashMap::default(),
            lookup_function_type: FastHashMap::default(),
            lookup_function: FastHashMap::default(),
        }
    }

    fn next(&mut self) -> Result<u32, Error> {
        self.data.next().ok_or(Error::IncompleteData)
    }

    fn next_inst(&mut self) -> Result<Instruction, Error> {
        let word = self.next()?;
        let (wc, opcode) = ((word >> 16) as u16, (word & 0xffff) as u16);
        if wc == 0 {
            return Err(Error::InvalidWordCount);
        }
        if opcode > LAST_KNOWN_OPCODE as u16 {
            return Err(Error::UnknownInstruction(opcode));
        }

        Ok(Instruction {
            op: unsafe {
                std::mem::transmute(opcode as u32)
            },
            wc,
        })
    }

    fn next_string(&mut self, mut count: u16) -> Result<(String, u16), Error>{
        self.temp_bytes.clear();
        loop {
            if count == 0 {
                return Err(Error::BadString);
            }
            count -= 1;
            let chars = self.next()?.to_le_bytes();
            let pos = chars.iter().position(|&c| c  == 0).unwrap_or(4);
            self.temp_bytes.extend_from_slice(&chars[.. pos]);
            if pos < 4 {
                break
            }
        }
        std::str::from_utf8(&self.temp_bytes)
            .map(|s| (s.to_owned(), count))
            .map_err(|_| Error::BadString)
    }

    fn next_decoration(
        &mut self,
        inst: Instruction,
        base_words: u16,
        dec: &mut Decoration,
    ) -> Result<(), Error> {
        let raw = self.next()?;
        if raw > LAST_KNOWN_DECORATION as spirv::Word {
            return Err(Error::InvalidDecoration(raw));
        }
        let dec_typed = unsafe {
            std::mem::transmute::<_, spirv::Decoration>(raw)
        };
        log::trace!("\t\t{:?}", dec_typed);
        match dec_typed {
            spirv::Decoration::BuiltIn => {
                inst.expect(base_words + 2)?;
                let raw = self.next()?;
                if raw > LAST_KNOWN_BUILT_IN as spirv::Word {
                    log::warn!("Unknown built in {:?}", raw);
                } else {
                    dec.built_in = Some(unsafe {
                        std::mem::transmute(raw)
                    });
                }
            }
            spirv::Decoration::Location => {
                inst.expect(base_words + 2)?;
                dec.location = Some(self.next()?);
            }
            spirv::Decoration::DescriptorSet => {
                inst.expect(base_words + 2)?;
                dec.desc_set = Some(self.next()?);
            }
            spirv::Decoration::Binding => {
                inst.expect(base_words + 2)?;
                dec.desc_index = Some(self.next()?);
            }
            other => {
                log::warn!("Unknown decoration {:?}", other);
                for _ in base_words + 1 .. inst.wc {
                    let _var = self.next()?;
                }
            }
        }
        Ok(())
    }

    fn next_block(
        &mut self,
        fun: &mut crate::Function,
        type_store: &Storage<crate::Type>,
        const_store: &Storage<crate::Constant>,
    ) -> Result<(), Error> {
        loop {
            use spirv::Op;
            let inst = self.next_inst()?;
            log::debug!("\t\t{:?} [{}]", inst.op, inst.wc);
            match inst.op {
                Op::AccessChain => {
                    struct AccessExpression {
                        base_token: Token<crate::Expression>,
                        type_id: spirv::Word,
                    }
                    inst.expect_at_least(4)?;
                    let result_type_id = self.next()?;
                    let result_id = self.next()?;
                    let base_id = self.next()?;
                    log::trace!("\t\t\tlooking up expr {:?}", base_id);
                    let mut acex = {
                        let expr = self.lookup_expression.lookup(base_id)?;
                        let ptr_type = self.lookup_type.lookup(expr.type_id)?;
                        AccessExpression {
                            base_token: expr.token,
                            type_id: ptr_type.base_id.unwrap(),
                        }
                    };
                    for _ in 4 .. inst.wc {
                        let access_id = self.next()?;
                        log::trace!("\t\t\tlooking up expr {:?}", access_id);
                        let index_expr = self.lookup_expression.lookup(access_id)?.clone();
                        let index_type_token = self.lookup_type.lookup(index_expr.type_id)?.token;
                        match type_store[index_type_token].inner {
                            crate::TypeInner::Scalar { kind: crate::ScalarKind::Uint, .. } |
                            crate::TypeInner::Scalar { kind: crate::ScalarKind::Sint, .. } => (),
                            _ => return Err(Error::UnsupportedType(index_type_token)),
                        }
                        log::trace!("\t\t\tlooking up type {:?}", acex.type_id);
                        let type_lookup = self.lookup_type.lookup(acex.type_id)?;
                        acex = match type_store[type_lookup.token].inner {
                            crate::TypeInner::Struct { .. } => {
                                let index = match fun.expressions[index_expr.token] {
                                    crate::Expression::Constant(const_token) => {
                                        match const_store[const_token].inner {
                                            crate::ConstantInner::Uint(v) => v as u32,
                                            crate::ConstantInner::Sint(v) => v as u32,
                                            _ => return Err(Error::InvalidAccessIndex(index_expr.token)),
                                        }
                                    }
                                    _ => return Err(Error::InvalidAccessIndex(index_expr.token))
                                };
                                AccessExpression {
                                    base_token: fun.expressions.append(crate::Expression::AccessIndex {
                                        base: acex.base_token,
                                        index,
                                    }),
                                    type_id: *self.lookup_member_type_id
                                        .get(&(acex.type_id, index))
                                        .ok_or(Error::InvalidAccessType(acex.type_id))?,
                                }
                            }
                            crate::TypeInner::Array { .. } |
                            crate::TypeInner::Vector { .. } |
                            crate::TypeInner::Matrix { .. } => {
                                AccessExpression {
                                    base_token: fun.expressions.append(crate::Expression::Access {
                                        base: acex.base_token,
                                        index: index_expr.token,
                                    }),
                                    type_id: type_lookup.base_id
                                        .ok_or(Error::InvalidAccessType(acex.type_id))?,
                                }
                            }
                            _ => return Err(Error::UnsupportedType(type_lookup.token)),
                        };
                    }

                    self.lookup_expression.insert(result_id, LookupExpression {
                        token: acex.base_token,
                        type_id: result_type_id,
                    });
                }
                Op::CompositeExtract => {
                    inst.expect_at_least(4)?;
                    let result_type_id = self.next()?;
                    let result_id = self.next()?;
                    let base_id = self.next()?;
                    log::trace!("\t\t\tlooking up expr {:?}", base_id);
                    let mut lexp = {
                        let expr = self.lookup_expression.lookup(base_id)?;
                        LookupExpression {
                            token: expr.token,
                            type_id: expr.type_id,
                        }
                    };
                    for _ in 4 .. inst.wc {
                        let index = self.next()?;
                        log::trace!("\t\t\tlooking up type {:?}", lexp.type_id);
                        let type_lookup = self.lookup_type.lookup(lexp.type_id)?;
                        let type_id = match type_store[type_lookup.token].inner {
                            crate::TypeInner::Struct { .. } => {
                                *self.lookup_member_type_id
                                    .get(&(lexp.type_id, index))
                                    .ok_or(Error::InvalidAccessType(lexp.type_id))?
                            }
                            crate::TypeInner::Array { .. } |
                            crate::TypeInner::Vector { .. } |
                            crate::TypeInner::Matrix { .. } => {
                                type_lookup.base_id
                                    .ok_or(Error::InvalidAccessType(lexp.type_id))?
                            }
                            _ => return Err(Error::UnsupportedType(type_lookup.token)),
                        };
                        lexp = LookupExpression {
                            token: fun.expressions.append(crate::Expression::AccessIndex {
                                base: lexp.token,
                                index,
                            }),
                            type_id,
                        };
                    }

                    self.lookup_expression.insert(result_id, LookupExpression {
                        token: lexp.token,
                        type_id: result_type_id,
                    });
                }
                Op::CompositeConstruct => {
                    inst.expect_at_least(3)?;
                    let result_type_id = self.next()?;
                    let id = self.next()?;
                    let mut components = Vec::with_capacity(inst.wc as usize  - 2);
                    for _ in 3 .. inst.wc {
                        let comp_id = self.next()?;
                        log::trace!("\t\t\tlooking up expr {:?}", comp_id);
                        let lexp = self.lookup_expression.lookup(comp_id)?;
                        components.push(lexp.token);
                    }
                    let expr = crate::Expression::Compose {
                        ty: self.lookup_type.lookup(result_type_id)?.token,
                        components,
                    };
                    self.lookup_expression.insert(id, LookupExpression {
                        token: fun.expressions.append(expr),
                        type_id: result_type_id,
                    });
                }
                Op::Load => {
                    inst.expect_at_least(4)?;
                    let result_type_id = self.next()?;
                    let result_id = self.next()?;
                    let pointer_id = self.next()?;
                    if inst.wc != 4 {
                        inst.expect(5)?;
                        let _memory_access = self.next()?;
                    }
                    let base_expr = self.lookup_expression.lookup(pointer_id)?;
                    let base_type = self.lookup_type.lookup(base_expr.type_id)?;
                    if base_type.base_id != Some(result_type_id) {
                        return Err(Error::InvalidLoadType(result_type_id));
                    }
                    match type_store[base_type.token].inner {
                        crate::TypeInner::Pointer { .. } => (),
                        _ => return Err(Error::UnsupportedType(base_type.token)),
                    }
                    let expr = crate::Expression::Load {
                        pointer: base_expr.token,
                    };
                    self.lookup_expression.insert(result_id, LookupExpression {
                        token: fun.expressions.append(expr),
                        type_id: result_type_id,
                    });
                }
                Op::Store => {
                    inst.expect_at_least(3)?;
                    let pointer_id = self.next()?;
                    let value_id = self.next()?;
                    if inst.wc != 3 {
                        inst.expect(4)?;
                        let _memory_access = self.next()?;
                    }
                    let base_expr = self.lookup_expression.lookup(pointer_id)?;
                    let base_type = self.lookup_type.lookup(base_expr.type_id)?;
                    match type_store[base_type.token].inner {
                        crate::TypeInner::Pointer { .. } => (),
                        _ => return Err(Error::UnsupportedType(base_type.token)),
                    };
                    let value_expr = self.lookup_expression.lookup(value_id)?;
                    if base_type.base_id != Some(value_expr.type_id) {
                        return Err(Error::InvalidStoreType(value_expr.type_id));
                    }
                    fun.body.push(crate::Statement::Store {
                        pointer: base_expr.token,
                        value: value_expr.token,
                    })
                }
                Op::Return => {
                    inst.expect(1)?;
                    fun.body.push(crate::Statement::Return { value: None });
                    break
                }
                Op::VectorTimesScalar => {
                    inst.expect(5)?;
                    let result_type_id = self.next()?;
                    let result_type_loookup = self.lookup_type.lookup(result_type_id)?;
                    let (res_size, res_width) = match type_store[result_type_loookup.token].inner {
                        crate::TypeInner::Vector { size, kind: crate::ScalarKind::Float, width } => (size, width),
                        _ => return Err(Error::UnsupportedType(result_type_loookup.token)),
                    };
                    let result_id = self.next()?;
                    let vector_id = self.next()?;
                    let scalar_id = self.next()?;
                    let vector_lexp = self.lookup_expression.lookup(vector_id)?;
                    let vector_type_lookup = self.lookup_type.lookup(vector_lexp.type_id)?;
                    match type_store[vector_type_lookup.token].inner {
                        crate::TypeInner::Vector { size, kind: crate::ScalarKind::Float, width } if size == res_size && width == res_width => (),
                        _ => return Err(Error::UnsupportedType(vector_type_lookup.token)),
                    };
                    let scalar_lexp = self.lookup_expression.lookup(scalar_id)?.clone();
                    let scalar_type_lookup = self.lookup_type.lookup(scalar_lexp.type_id)?;
                    match type_store[scalar_type_lookup.token].inner {
                        crate::TypeInner::Scalar { kind: crate::ScalarKind::Float, width } if width == res_width => (),
                        _ => return Err(Error::UnsupportedType(scalar_type_lookup.token)),
                    };
                    let expr = crate::Expression::Mul(vector_lexp.token, scalar_lexp.token);
                    self.lookup_expression.insert(result_id, LookupExpression {
                        token: fun.expressions.append(expr),
                        type_id: result_type_id,
                    });
                }
                Op::MatrixTimesVector => {
                    inst.expect(5)?;
                    let result_type_id = self.next()?;
                    let result_type_loookup = self.lookup_type.lookup(result_type_id)?;
                    let (res_size, res_width) = match type_store[result_type_loookup.token].inner {
                        crate::TypeInner::Vector { size, kind: crate::ScalarKind::Float, width } => (size, width),
                        _ => return Err(Error::UnsupportedType(result_type_loookup.token)),
                    };
                    let result_id = self.next()?;
                    let matrix_id = self.next()?;
                    let vector_id = self.next()?;
                    let matrix_lexp = self.lookup_expression.lookup(matrix_id)?;
                    let matrix_type_lookup = self.lookup_type.lookup(matrix_lexp.type_id)?;
                    let columns = match type_store[matrix_type_lookup.token].inner {
                        crate::TypeInner::Matrix { columns, rows, kind: crate::ScalarKind::Float, width } if rows == res_size && width == res_width => columns,
                        _ => return Err(Error::UnsupportedType(matrix_type_lookup.token)),
                    };
                    let vector_lexp = self.lookup_expression.lookup(vector_id)?.clone();
                    let vector_type_lookup = self.lookup_type.lookup(vector_lexp.type_id)?;
                    match type_store[vector_type_lookup.token].inner {
                        crate::TypeInner::Vector { size, kind: crate::ScalarKind::Float, width } if size == columns && width == res_width => (),
                        _ => return Err(Error::UnsupportedType(vector_type_lookup.token)),
                    };
                    let expr = crate::Expression::Mul(matrix_lexp.token, vector_lexp.token);
                    self.lookup_expression.insert(result_id, LookupExpression {
                        token: fun.expressions.append(expr),
                        type_id: result_type_id,
                    });
                }
                Op::SampledImage => {
                    inst.expect(5)?;
                    let _result_type_id = self.next()?;
                    let result_id = self.next()?;
                    let image_id = self.next()?;
                    let sampler_id = self.next()?;
                    let image_lexp = self.lookup_expression.lookup(image_id)?;
                    let sampler_lexp = self.lookup_expression.lookup(sampler_id)?;
                    //TODO: compare the result type
                    self.lookup_sampled_image.insert(result_id, LookupSampledImage {
                        image: image_lexp.token,
                        sampler: sampler_lexp.token,
                    });
                }
                Op::ImageSampleImplicitLod => {
                    inst.expect_at_least(5)?;
                    let result_type_id = self.next()?;
                    let result_id = self.next()?;
                    let sampled_image_id = self.next()?;
                    let coordinate_id = self.next()?;
                    let si_lexp = self.lookup_sampled_image.lookup(sampled_image_id)?;
                    let coord_lexp = self.lookup_expression.lookup(coordinate_id)?;
                    let coord_type_lookup = self.lookup_type.lookup(coord_lexp.type_id)?;
                    match type_store[coord_type_lookup.token].inner {
                        crate::TypeInner::Scalar { kind: crate::ScalarKind::Float, .. } |
                        crate::TypeInner::Vector { kind: crate::ScalarKind::Float, .. } => (),
                        _ => return Err(Error::UnsupportedType(coord_type_lookup.token)),
                    }
                    //TODO: compare the result type
                    let expr = crate::Expression::ImageSample {
                        image: si_lexp.image,
                        sampler: si_lexp.sampler,
                        coordinate: coord_lexp.token,
                    };
                    self.lookup_expression.insert(result_id, LookupExpression {
                        token: fun.expressions.append(expr),
                        type_id: result_type_id,
                    });
                }
                _ => return Err(Error::UnsupportedInstruction(self.state, inst.op)),
            }
        }
        Ok(())
    }

    fn make_expression_storage(&mut self) -> Storage<crate::Expression> {
        let mut expressions = Storage::new();
        assert!(self.lookup_expression.is_empty());
        // register global variables
        for (&id, var) in self.lookup_variable.iter() {
            self.lookup_expression.insert(id, LookupExpression {
                type_id: var.type_id,
                token: expressions.append(crate::Expression::GlobalVariable(var.token)),
            });
        }
        // register constants
        for (&id, con) in self.lookup_constant.iter() {
            self.lookup_expression.insert(id, LookupExpression {
                type_id: con.type_id,
                token: expressions.append(crate::Expression::Constant(con.token)),
            });
        }
        // done
        expressions
    }

    fn switch(&mut self, state: ModuleState, op: spirv::Op) -> Result<(), Error> {
        if state < self.state {
            return Err(Error::UnsupportedInstruction(self.state, op))
        } else {
            self.state = state;
            Ok(())
        }
    }

    pub fn parse(&mut self) -> Result<crate::Module, Error> {
        let mut module = crate::Module::from_header({
            if self.next()? != spirv::MAGIC_NUMBER {
                return Err(Error::InvalidHeader);
            }
            let version_raw = self.next()?.to_le_bytes();
            let generator = self.next()?;
            let _bound = self.next()?;
            let _schema = self.next()?;
            crate::Header {
                version: (version_raw[2], version_raw[1], version_raw[0]),
                generator,
            }
        });
        let mut entry_points = Vec::new();

        while let Ok(inst) = self.next_inst() {
            use spirv::Op;
            log::debug!("\t{:?} [{}]", inst.op, inst.wc);
            match inst.op {
                Op::Capability => {
                    self.switch(ModuleState::Capability, inst.op)?;
                    inst.expect(2)?;
                    let capability = self.next()?;
                    if capability > LAST_KNOWN_CAPABILITY as u32 {
                        return Err(Error::UnknownCapability(capability));
                    }
                    let cap = unsafe {
                        std::mem::transmute(capability)
                    };
                    if !SUPPORTED_CAPABILITIES.contains(&cap) {
                        return Err(Error::UnsupportedCapability(cap));
                    }
                }
                Op::Extension => {
                    self.switch(ModuleState::Extension, inst.op)?;
                    inst.expect_at_least(2)?;
                    let (name, left) = self.next_string(inst.wc - 1)?;
                    if left != 0 {
                        return Err(Error::InvalidOperand);
                    }
                    if !SUPPORTED_EXTENSIONS.contains(&name.as_str()) {
                        return Err(Error::UnsupportedExtension(name.to_owned()));
                    }
                }
                Op::ExtInstImport => {
                    self.switch(ModuleState::Extension, inst.op)?;
                    inst.expect_at_least(3)?;
                    let _result = self.next()?;
                    let (name, left) = self.next_string(inst.wc - 2)?;
                    if left != 0 {
                        return Err(Error::InvalidOperand)
                    }
                    if !SUPPORTED_EXT_SETS.contains(&name.as_str()) {
                        return Err(Error::UnsupportedExtSet(name.to_owned()));
                    }
                }
                Op::MemoryModel => {
                    self.switch(ModuleState::MemoryModel, inst.op)?;
                    inst.expect(3)?;
                    let _addressing_model = self.next()?;
                    let _memory_model = self.next()?;
                }
                Op::EntryPoint => {
                    self.switch(ModuleState::EntryPoint, inst.op)?;
                    inst.expect_at_least(4)?;
                    let exec_model = self.next()?;
                    if exec_model > LAST_KNOWN_EXECUTION_MODEL as u32 {
                        return Err(Error::UnsupportedExecutionModel(exec_model));
                    }
                    let function_id = self.next()?;
                    let (name, left) = self.next_string(inst.wc - 3)?;
                    let ep = EntryPoint {
                        exec_model: unsafe {
                            std::mem::transmute(exec_model)
                        },
                        name: name.to_owned(),
                        function_id,
                        variable_ids: self.data
                            .by_ref()
                            .take(left as usize)
                            .collect(),
                    };
                    entry_points.push(ep);
                }
                Op::ExecutionMode => {
                    self.switch(ModuleState::ExecutionMode, inst.op)?;
                    inst.expect_at_least(3)?;
                    let _ep_id = self.next()?;
                    let _mode = self.next()?;
                    for _ in 3 .. inst.wc {
                        let _ = self.next()?; //TODO
                    }
                }
                Op::Source => {
                    self.switch(ModuleState::Source, inst.op)?;
                    for _ in 1 .. inst.wc {
                        let _ = self.next()?;
                    }
                }
                Op::SourceExtension => {
                    self.switch(ModuleState::Source, inst.op)?;
                    inst.expect_at_least(2)?;
                    let (_name, _) = self.next_string(inst.wc - 1)?;
                }
                Op::Name => {
                    self.switch(ModuleState::Name, inst.op)?;
                    inst.expect_at_least(3)?;
                    let id = self.next()?;
                    let (name, left) = self.next_string(inst.wc - 2)?;
                    if left != 0 {
                        return Err(Error::InvalidOperand);
                    }
                    self.future_decor
                        .entry(id)
                        .or_default()
                        .name = Some(name.to_owned());
                }
                Op::MemberName => {
                    self.switch(ModuleState::Name, inst.op)?;
                    inst.expect_at_least(4)?;
                    let id = self.next()?;
                    let member = self.next()?;
                    let (name, left) = self.next_string(inst.wc - 3)?;
                    if left != 0 {
                        return Err(Error::InvalidOperand);
                    }
                    self.future_member_decor
                        .entry((id, member))
                        .or_default()
                        .name = Some(name.to_owned());
                }
                Op::Decorate => {
                    self.switch(ModuleState::Annotation, inst.op)?;
                    inst.expect_at_least(3)?;
                    let id = self.next()?;
                    let mut dec = self.future_decor
                        .remove(&id)
                        .unwrap_or_default();
                    self.next_decoration(inst, 2, &mut dec)?;
                    self.future_decor.insert(id, dec);
                }
                Op::MemberDecorate => {
                    self.switch(ModuleState::Annotation, inst.op)?;
                    inst.expect_at_least(4)?;
                    let id = self.next()?;
                    let member = self.next()?;
                    let mut dec = self.future_member_decor
                        .remove(&(id, member))
                        .unwrap_or_default();
                    self.next_decoration(inst, 3, &mut dec)?;
                    self.future_member_decor.insert((id, member), dec);
                }
                Op::TypeVoid => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect(2)?;
                    let id = self.next()?;
                    self.lookup_void_type.insert(id);
                }
                Op::TypeInt => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect(4)?;
                    let id = self.next()?;
                    let width = self.next()?;
                    let sign = self.next()?;
                    let inner = crate::TypeInner::Scalar {
                        kind: match sign {
                            0 => crate::ScalarKind::Uint,
                            1 => crate::ScalarKind::Sint,
                            _ => return Err(Error::InvalidSign(sign)),
                        },
                        width: width
                            .try_into()
                            .map_err(|_| Error::InvalidTypeWidth(width))?,
                    };
                    self.lookup_type.insert(id, LookupType {
                        token: module.types.append(crate::Type {
                            name: self.future_decor
                                .remove(&id)
                                .and_then(|dec| dec.name),
                            inner,
                        }),
                        base_id: None,
                    });
                }
                Op::TypeFloat => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect(3)?;
                    let id = self.next()?;
                    let width = self.next()?;
                    let inner = crate::TypeInner::Scalar {
                        kind: crate::ScalarKind::Float,
                        width: width
                            .try_into()
                            .map_err(|_| Error::InvalidTypeWidth(width))?,
                    };
                    self.lookup_type.insert(id, LookupType {
                        token: module.types.append(crate::Type {
                            name: self.future_decor
                                .remove(&id)
                                .and_then(|dec| dec.name),
                            inner,
                        }),
                        base_id: None,
                    });
                }
                Op::TypeVector => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect(4)?;
                    let id = self.next()?;
                    let type_id = self.next()?;
                    let type_lookup = self.lookup_type.lookup(type_id)?;
                    let (kind, width) = match module.types[type_lookup.token].inner {
                        crate::TypeInner::Scalar { kind, width } => (kind, width),
                        _ => return Err(Error::InvalidInnerType(type_id)),
                    };
                    let component_count = self.next()?;
                    let inner = crate::TypeInner::Vector {
                        size: map_vector_size(component_count)?,
                        kind,
                        width,
                    };
                    self.lookup_type.insert(id, LookupType {
                        token: module.types.append(crate::Type {
                            name: self.future_decor
                                .remove(&id)
                                .and_then(|dec| dec.name),
                            inner,
                        }),
                        base_id: Some(type_id),
                    });
                }
                Op::TypeMatrix => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect(4)?;
                    let id = self.next()?;
                    let vector_type_id = self.next()?;
                    let num_columns = self.next()?;
                    let vector_type_lookup = self.lookup_type.lookup(vector_type_id)?;
                    let inner = match module.types[vector_type_lookup.token].inner {
                        crate::TypeInner::Vector { size, kind, width } => crate::TypeInner::Matrix {
                            columns: map_vector_size(num_columns)?,
                            rows: size,
                            kind,
                            width,
                        },
                        _ => return Err(Error::InvalidInnerType(vector_type_id)),
                    };
                    self.lookup_type.insert(id, LookupType {
                        token: module.types.append(crate::Type {
                            name: self.future_decor
                                .remove(&id)
                                .and_then(|dec| dec.name),
                            inner,
                        }),
                        base_id: Some(vector_type_id),
                    });
                }
                Op::TypeFunction => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect_at_least(3)?;
                    let id = self.next()?;
                    let return_type_id = self.next()?;
                    let parameter_type_ids = self.data
                        .by_ref()
                        .take(inst.wc as usize - 3)
                        .collect();
                    self.lookup_function_type.insert(id, LookupFunctionType {
                        parameter_type_ids,
                        return_type_id,
                    });
                }
                Op::TypePointer => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect(4)?;
                    let id = self.next()?;
                    let storage = self.next()?;
                    let type_id = self.next()?;
                    let inner = crate::TypeInner::Pointer {
                        base: self.lookup_type.lookup(type_id)?.token,
                        class: map_storage_class(storage)?,
                    };
                    self.lookup_type.insert(id, LookupType {
                        token: module.types.append(crate::Type {
                            name: self.future_decor
                                .remove(&id)
                                .and_then(|dec| dec.name),
                            inner,
                        }),
                        base_id: Some(type_id),
                    });
                }
                Op::TypeArray => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect(4)?;
                    let id = self.next()?;
                    let type_id = self.next()?;
                    let length = self.next()?;
                    let inner = crate::TypeInner::Array {
                        base: self.lookup_type.lookup(type_id)?.token,
                        size: crate::ArraySize::Static(length),
                    };
                    self.lookup_type.insert(id, LookupType {
                        token: module.types.append(crate::Type {
                            name: self.future_decor
                                .remove(&id)
                                .and_then(|dec| dec.name),
                            inner,
                        }),
                        base_id: Some(type_id),
                    });
                }
                Op::TypeRuntimeArray => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect(4)?;
                    let id = self.next()?;
                    let type_id = self.next()?;
                    let inner = crate::TypeInner::Array {
                        base: self.lookup_type.lookup(type_id)?.token,
                        size: crate::ArraySize::Dynamic,
                    };
                    self.lookup_type.insert(id, LookupType {
                        token: module.types.append(crate::Type {
                            name: self.future_decor
                                .remove(&id)
                                .and_then(|dec| dec.name),
                            inner,
                        }),
                        base_id: Some(type_id),
                    });
                }
                Op::TypeStruct => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect_at_least(2)?;
                    let id = self.next()?;
                    let mut members = Vec::with_capacity(inst.wc as usize - 2);
                    for i in 0 .. inst.wc as u32 - 2 {
                        let type_id = self.next()?;
                        let ty = self.lookup_type.lookup(type_id)?.token;
                        self.lookup_member_type_id.insert((id, i), type_id);
                        let decor = self.future_member_decor
                            .remove(&(id, i))
                            .unwrap_or_default();
                        let binding = decor.get_binding();
                        members.push(crate::StructMember {
                            name: decor.name,
                            binding,
                            ty,
                        });
                    }
                    let inner = crate::TypeInner::Struct {
                        members
                    };
                    self.lookup_type.insert(id, LookupType {
                        token: module.types.append(crate::Type {
                            name: self.future_decor
                                .remove(&id)
                                .and_then(|dec| dec.name),
                            inner,
                        }),
                        base_id: None,
                    });
                }
                Op::TypeImage => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect_at_least(9)?;

                    let id = self.next()?;
                    let sample_type_id = self.next()?;
                    let dim = self.next()?;
                    let mut flags = crate::ImageFlags::empty();
                    let _is_depth = self.next()?;
                    if self.next()? != 0 {
                        flags |= crate::ImageFlags::ARRAYED;
                    }
                    if self.next()? != 0 {
                        flags |= crate::ImageFlags::MULTISAMPLED;
                    }
                    let is_sampled = self.next()?;
                    if is_sampled != 0 {
                        flags |= crate::ImageFlags::SAMPLED;
                    }
                    let _format = self.next()?;
                    if inst.wc > 9 {
                        inst.expect(10)?;
                        let access = self.next()?;
                        if access == 0 || access == 2 {
                            flags |= crate::ImageFlags::CAN_LOAD;
                        }
                        if access == 1 || access == 2 {
                            flags |= crate::ImageFlags::CAN_STORE;
                        }
                    };

                    let decor = self.future_decor
                        .remove(&id)
                        .unwrap_or_default();

                    let inner = crate::TypeInner::Image {
                        base: self.lookup_type.lookup(sample_type_id)?.token,
                        dim: if dim > LAST_KNOWN_DIM as u32 {
                            return Err(Error::UnsupportedDim(dim));
                        } else {
                            unsafe { std::mem::transmute(dim) }
                        },
                        flags,
                    };
                    self.lookup_type.insert(id, LookupType {
                        token: module.types.append(crate::Type {
                            name: decor.name,
                            inner,
                        }),
                        base_id: Some(sample_type_id),
                    });
                }
                Op::TypeSampledImage => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect(3)?;
                    let id = self.next()?;
                    let image_id = self.next()?;
                    self.lookup_type.insert(id, LookupType {
                        token: self.lookup_type.lookup(image_id)?.token,
                        base_id: Some(image_id),
                    });
                }
                Op::TypeSampler => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect(2)?;
                    let id = self.next()?;
                    let decor = self.future_decor
                        .remove(&id)
                        .unwrap_or_default();
                    let inner = crate::TypeInner::Sampler;
                    self.lookup_type.insert(id, LookupType {
                        token: module.types.append(crate::Type {
                            name: decor.name,
                            inner,
                        }),
                        base_id: None,
                    });
                }
                Op::Constant |
                Op::SpecConstant => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect_at_least(3)?;
                    let type_id = self.next()?;
                    let id = self.next()?;
                    let type_lookup = self.lookup_type.lookup(type_id)?;
                    let inner = match module.types[type_lookup.token].inner {
                        crate::TypeInner::Scalar { kind: crate::ScalarKind::Uint, width } => {
                            let low = self.next()?;
                            let high = if width > 32 {
                                inst.expect(4)?;
                                self.next()?
                            } else {
                                0
                            };
                            crate::ConstantInner::Uint(((high as u64) << 32) | low as u64)
                        }
                        crate::TypeInner::Scalar { kind: crate::ScalarKind::Sint, width } => {
                            let low = self.next()?;
                            let high = if width < 32 {
                                return Err(Error::InvalidTypeWidth(width as u32));
                            } else if width > 32 {
                                inst.expect(4)?;
                                self.next()?
                            } else {
                                !0
                            };
                            crate::ConstantInner::Sint(unsafe {
                                std::mem::transmute(((high as u64) << 32) | low as u64)
                            })
                        }
                        crate::TypeInner::Scalar { kind: crate::ScalarKind::Float, width } => {
                            let low = self.next()?;
                            let extended = if width < 32 {
                                return Err(Error::InvalidTypeWidth(width as u32));
                            } else if width > 32 {
                                inst.expect(4)?;
                                let high = self.next()?;
                                unsafe {
                                    std::mem::transmute(((high as u64) << 32) | low as u64)
                                }
                            } else {
                                unsafe {
                                    std::mem::transmute::<_, f32>(low) as f64
                                }
                            };
                            crate::ConstantInner::Float(extended)
                        }
                        _ => return Err(Error::UnsupportedType(type_lookup.token))
                    };
                    self.lookup_constant.insert(id, LookupConstant {
                        token: module.constants.append(crate::Constant {
                            name: self.future_decor
                                .remove(&id)
                                .and_then(|dec| dec.name),
                            specialization: None, //TODO
                            inner,
                        }),
                        type_id,
                    });
                }
                Op::Variable => {
                    self.switch(ModuleState::Type, inst.op)?;
                    inst.expect_at_least(4)?;
                    let type_id = self.next()?;
                    let id = self.next()?;
                    let storage = self.next()?;
                    if inst.wc != 4 {
                        inst.expect(5)?;
                        let _init = self.next()?; //TODO
                    }
                    let lookup_type = self.lookup_type.lookup(type_id)?;
                    let dec = self.future_decor
                        .remove(&id)
                        .ok_or(Error::InvalidBinding(id))?;
                    let binding = match module.types[lookup_type.token].inner {
                        crate::TypeInner::Pointer { base, class: spirv::StorageClass::Input } |
                        crate::TypeInner::Pointer { base, class: spirv::StorageClass::Output } => {
                            match module.types[base].inner {
                                crate::TypeInner::Struct { ref members } => {
                                    // we don't expect binding decoration on I/O structs,
                                    // but we do expect them on all of the members
                                    for member in members {
                                        if member.binding.is_none() {
                                            log::warn!("Struct {:?} member {:?} doesn't have a binding", base, member);
                                            return Err(Error::InvalidBinding(id));
                                        }
                                    }
                                    None
                                }
                                _ => {
                                    Some(dec
                                        .get_binding()
                                        .ok_or(Error::InvalidBinding(id))?
                                    )
                                }
                            }
                        }
                        _ => {
                            Some(dec
                                .get_binding()
                                .ok_or(Error::InvalidBinding(id))?
                            )
                        }
                    };
                    let var = crate::GlobalVariable {
                        name: dec.name,
                        class: map_storage_class(storage)?,
                        binding,
                        ty: lookup_type.token,
                    };
                    let token = module.global_variables.append(var);
                    self.lookup_variable.insert(id, LookupVariable {
                        token,
                        type_id,
                    });
                }
                Op::Function => {
                    self.switch(ModuleState::Function, inst.op)?;
                    inst.expect(5)?;
                    let result_type = self.next()?;
                    let fun_id = self.next()?;
                    let fun_control = self.next()?;
                    let fun_type = self.next()?;
                    let mut fun = {
                        let ft = self.lookup_function_type.lookup(fun_type)?.clone();
                        if ft.return_type_id != result_type {
                            return Err(Error::WrongFunctionResultType(result_type))
                        }
                        crate::Function {
                            name: self.future_decor
                                .remove(&fun_id)
                                .and_then(|dec| dec.name),
                            control: spirv::FunctionControl::from_bits(fun_control)
                                .ok_or(Error::UnsupportedFunctionControl(fun_control))?,
                            parameter_types: Vec::with_capacity(ft.parameter_type_ids.len()),
                            return_type: if self.lookup_void_type.contains(&result_type) {
                                None
                            } else {
                                Some(self.lookup_type.lookup(result_type)?.token)
                            },
                            expressions: self.make_expression_storage(),
                            body: Vec::new(),
                        }
                    };
                    // read parameters
                    for i in 0 .. fun.parameter_types.capacity() {
                        match self.next_inst()? {
                            Instruction { op: Op::FunctionParameter, wc: 3 } => {
                                let type_id = self.next()?;
                                let _id = self.next()?;
                                //Note: we redo the lookup in order to work around `self` borrowing
                                if type_id != self.lookup_function_type
                                    .lookup(fun_type)?
                                    .parameter_type_ids[i]
                                {
                                    return Err(Error::WrongFunctionParameterType(type_id))
                                }
                                let ty = self.lookup_type.lookup(type_id)?.token;
                                fun.parameter_types.push(ty);
                            }
                            Instruction { op, .. } => return Err(Error::InvalidParameter(op)),
                        }
                    }
                    // read body
                    loop {
                        let fun_inst = self.next_inst()?;
                        log::debug!("\t\t{:?}", fun_inst.op);
                        match fun_inst.op {
                            Op::Label => {
                                fun_inst.expect(2)?;
                                let _id = self.next()?;
                                self.next_block(&mut fun, &module.types, &module.constants)?;
                            }
                            Op::FunctionEnd => {
                                fun_inst.expect(1)?;
                                break
                            }
                            _ => return Err(Error::UnsupportedInstruction(self.state, fun_inst.op))
                        }
                    }
                    // done
                    let token = module.functions.append(fun);
                    self.lookup_function.insert(fun_id, token);
                    self.lookup_expression.clear();
                    self.lookup_sampled_image.clear();
                }
                _ => return Err(Error::UnsupportedInstruction(self.state, inst.op))
                //TODO
            }
        }

        if !self.future_decor.is_empty() {
            log::warn!("Unused item decorations: {:?}", self.future_decor);
            self.future_decor.clear();
        }
        if !self.future_member_decor.is_empty() {
            log::warn!("Unused member decorations: {:?}", self.future_member_decor);
            self.future_member_decor.clear();
        }

        module.entry_points.reserve(entry_points.len());
        for raw in entry_points {
            let mut ep = crate::EntryPoint {
                exec_model: raw.exec_model,
                name: raw.name,
                function: *self.lookup_function.lookup(raw.function_id)?,
                inputs: Vec::new(),
                outputs: Vec::new(),
            };
            for var_id in raw.variable_ids {
                let token = self.lookup_variable.lookup(var_id)?.token;
                match module.global_variables[token].class {
                    spirv::StorageClass::Input => ep.inputs.push(token),
                    spirv::StorageClass::Output => ep.outputs.push(token),
                    other => return Err(Error::InvalidVariableClass(other))
                }
            }
            module.entry_points.push(ep);
        }

        Ok(module)
    }
}

pub fn parse_u8_slice(data: &[u8]) -> Result<crate::Module, Error> {
    if data.len() % 4 != 0 {
        return Err(Error::IncompleteData);
    }

    let words = data
        .chunks(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()));
    Parser::new(words).parse()
}

#[cfg(test)]
mod test {
    #[test]
    fn parse() {
        let bin = vec![
            // Magic number.           Version number: 1.0.
            0x03, 0x02, 0x23, 0x07,    0x00, 0x00, 0x01, 0x00,
            // Generator number: 0.    Bound: 0.
            0x00, 0x00, 0x00, 0x00,    0x00, 0x00, 0x00, 0x00,
            // Reserved word: 0.
            0x00, 0x00, 0x00, 0x00,
            // OpMemoryModel.          Logical.
            0x0e, 0x00, 0x03, 0x00,    0x00, 0x00, 0x00, 0x00,
            // GLSL450.
            0x01, 0x00, 0x00, 0x00,
        ];
        let _ = super::parse_u8_slice(&bin).unwrap();
    }
}
