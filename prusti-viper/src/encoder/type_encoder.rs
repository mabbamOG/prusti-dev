// © 2019, ETH Zurich
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::encoder::foldunfold;
use crate::encoder::utils::range_extract;
use crate::encoder::utils::PlusOne;
use crate::encoder::Encoder;
use prusti_common::vir::{self, ExprIterator, ExprFolder};
use prusti_common::config;
// use prusti_interface::specifications::*;
// use rustc::middle::const_val::ConstVal;
use rustc_middle::ty;
use rustc_target::abi;
use rustc_middle::ty::layout::IntegerExt;
// use rustc_data_structures::indexed_vec::Idx;
// use std;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use rustc_ast::ast;
use prusti_interface::specs::typed;
use rustc_attr::IntType::SignedInt;
use rustc_target::abi::Integer;
use log::{debug, trace};
use crate::encoder::errors::{EncodingError, EncodingResult};

pub struct TypeEncoder<'p, 'v: 'p, 'tcx: 'v> {
    encoder: &'p Encoder<'v, 'tcx>,
    ty: ty::Ty<'tcx>,
}

impl<'p, 'v, 'r: 'v, 'tcx: 'v> TypeEncoder<'p, 'v, 'tcx> {
    pub fn new(encoder: &'p Encoder<'v, 'tcx>, ty: ty::Ty<'tcx>) -> Self {
        TypeEncoder { encoder, ty }
    }

    /// Is this type supported?
    fn is_supported_type(&self, ty: ty::Ty<'tcx>) -> bool {
        match ty.kind() {
            ty::TyKind::Bool
            | ty::TyKind::Int(_)
            | ty::TyKind::Uint(_)
            | ty::TyKind::Char
            | ty::TyKind::Ref(_, _, _)
            | ty::TyKind::Adt(_, _)
            | ty::TyKind::Tuple(_)
            | ty::TyKind::Never
            | ty::TyKind::Param(_) => true,
            _ => false,
        }
    }

    fn is_supported_subst(&self, subst: ty::subst::SubstsRef<'tcx>) -> bool {
        subst.iter().all(|kind| {
            if let ty::subst::GenericArgKind::Type(ty) = kind.unpack() {
                trace!(
                    "is_supported_subst({:?}) = {}",
                    ty,
                    self.is_supported_type(ty)
                );
                self.is_supported_type(ty)
            } else {
                true
            }
        })
    }

    /// Is this type supported?
    fn is_supported_field_type(&self, ty: ty::Ty<'tcx>) -> bool {
        match ty.kind() {
            ty::TyKind::Adt(_, subst) => self.is_supported_subst(subst),
            _ => self.is_supported_type(ty),
        }
    }

    /// Are all fields in the struct of a supported type?
    fn is_supported_struct_type(
        &self,
        adt_def: &ty::AdtDef,
        subst:ty::subst::SubstsRef<'tcx>,
    ) -> bool {
        let tcx = self.encoder.env().tcx();
        let supported_fields = adt_def.variants.iter().all(|variant| {
            variant.fields.iter().all(|field| {
                let field_ty = field.ty(tcx, subst);
                trace!(
                    "is_supported_type({:?}) = {}",
                    field_ty,
                    self.is_supported_type(field_ty)
                );
                self.is_supported_field_type(field_ty)
            })
        });
        supported_fields && self.is_supported_subst(subst)
    }

    pub fn encode_type(self) -> EncodingResult<vir::Type> {
        debug!("Encode type '{:?}'", self.ty);
        Ok(vir::Type::TypedRef(self.encode_predicate_use()?))
    }

    pub fn encode_value_type(self) -> EncodingResult<vir::Type> {
        debug!("Encode value type '{:?}'", self.ty);
        Ok(match self.ty.kind() {
            ty::TyKind::Bool => vir::Type::Bool,

            ty::TyKind::Int(_) | ty::TyKind::Uint(_) | ty::TyKind::Char => {
                vir::Type::Int
            }

            ty::TyKind::Ref(_, ref ty, _) => {
                let type_name = self.encoder.encode_type_predicate_use(ty)?;
                vir::Type::TypedRef(type_name)
            }

            ty::TyKind::Adt(_, _) | ty::TyKind::Tuple(_) => {
                let snapshot = self.encoder.encode_snapshot(&self.ty)?;
                if snapshot.is_defined() {
                    snapshot.get_type()
                } else {
                    return Err(EncodingError::internal(
                        format!(
                            "unexpected failure while encoding the snapshot of the '{:?}' type",
                            self.ty
                        )
                    ))
                }
            }

            ty::TyKind::RawPtr(ty::TypeAndMut { ref ty, .. }) => {
                return Err(EncodingError::unsupported(
                    "raw pointers are not supported"
                ));
            }

            ref x => unimplemented!("{:?}", x),
        })
    }


    /// provides the type of the underlying value or a reference in case of composed
    /// data structures
    pub fn encode_value_or_ref_type(self) -> EncodingResult<vir::Type> {
        debug!("Encode ref value type '{:?}'", self.ty);
        match self.ty.kind() {
            ty::TyKind::Adt(_, _)
            | ty::TyKind::Tuple(_) => {
                let snapshot = self.encoder.encode_snapshot(&self.ty)?;
                if snapshot.is_defined() {
                    let type_name = self.encoder
                        .encode_type_predicate_use(self.ty)?;
                    Ok(vir::Type::TypedRef(type_name))
                } else {
                    Err(EncodingError::internal(
                        format!(
                            "unexpected failure while encoding the snapshot of the '{:?}' type",
                            self.ty
                        )
                    ))
                }
            },

            _ => self.encode_value_type(),
        }
    }

    pub fn encode_value_field(self) -> EncodingResult<vir::Field> {
        trace!("Encode value field for type '{:?}'", self.ty);
        Ok(match self.ty.kind() {
            ty::TyKind::Bool => vir::Field::new("val_bool", vir::Type::Bool),

            ty::TyKind::Int(_) | ty::TyKind::Uint(_) | ty::TyKind::Char => {
                vir::Field::new("val_int", vir::Type::Int)
            }

            ty::TyKind::Ref(_, ref ty, _) => {
                let type_name = self.encoder.encode_type_predicate_use(ty)?;
                vir::Field::new("val_ref", vir::Type::TypedRef(type_name))
            }

            // For composed data structures, we typically use a snapshot rather than a field.
            // To unify how parameters are passed to functions, we treat them like a reference.
            ty::TyKind::Adt(_, _)
            | ty::TyKind::Tuple(_)
            | ty::TyKind::Closure(_, _)
            | ty::TyKind::FnDef(_, _) => {
                let type_name = self.encoder.encode_type_predicate_use(self.ty)?;
                vir::Field::new("val_ref", vir::Type::TypedRef(type_name))
            }

            ty::TyKind::RawPtr(ty::TypeAndMut { ref ty, .. }) => {
                return Err(EncodingError::unsupported(
                    "raw pointers are not supported"
                ));
            }

            ref x => unimplemented!("{:?}", x),
        })
    }

    fn get_integer_bounds(&self) -> Option<(vir::Expr, vir::Expr)> {
        match self.ty.kind() {
            ty::TyKind::Int(int_ty) => {
                let bounds = match int_ty {
                    ast::IntTy::I8 => (std::i8::MIN.into(), std::i8::MAX.into()),
                    ast::IntTy::I16 => (std::i16::MIN.into(), std::i16::MAX.into()),
                    ast::IntTy::I32 => (std::i32::MIN.into(), std::i32::MAX.into()),
                    ast::IntTy::I64 => (std::i64::MIN.into(), std::i64::MAX.into()),
                    ast::IntTy::I128 => (std::i128::MIN.into(), std::i128::MAX.into()),
                    ast::IntTy::Isize => (std::isize::MIN.into(), std::isize::MAX.into()),
                };
                Some(bounds)
            }
            ty::TyKind::Uint(uint_ty) => {
                let bounds = match uint_ty {
                    ast::UintTy::U8 => (0.into(), std::u8::MAX.into()),
                    ast::UintTy::U16 => (0.into(), std::u16::MAX.into()),
                    ast::UintTy::U32 => (0.into(), std::u32::MAX.into()),
                    ast::UintTy::U64 => (0.into(), std::u64::MAX.into()),
                    ast::UintTy::U128 => (0.into(), std::u128::MAX.into()),
                    ast::UintTy::Usize => (0.into(), std::usize::MAX.into()),
                };
                Some(bounds)
            }
            ty::TyKind::Char => {
                Some((0.into(), std::char::MAX.into()))
            }
            ty::TyKind::Bool | ty::TyKind::Ref(_, _, _) => None,
            ref x => unreachable!("{:?}", x),
        }
    }

    pub fn encode_bounds(self, var: &vir::Expr) -> Vec<vir::Expr> {
        if let Some((lower, upper)) = self.get_integer_bounds() {
            vec![
                vir::Expr::le_cmp(lower, var.clone()),
                vir::Expr::le_cmp(var.clone(), upper),
            ]
        } else {
            Vec::new()
        }
    }

    pub fn encode_predicate_def(self) -> EncodingResult<Vec<vir::Predicate>> {
        debug!("Encode type predicate '{:?}'", self.ty);
        let predicate_name = self.encoder.encode_type_predicate_use(self.ty)?;
        let typ = vir::Type::TypedRef(predicate_name.clone());

        Ok(match self.ty.kind() {
            ty::TyKind::Bool => vec![vir::Predicate::new_primitive_value(
                typ,
                self.encoder.encode_value_field(self.ty),
                None,
                false,
            )],

            ty::TyKind::Int(_) | ty::TyKind::Uint(_) | ty::TyKind::Char => {
                let bounds = if config::check_overflows() {
                    self.get_integer_bounds()
                } else {
                    None
                };
                let unsigned = if let ty::TyKind::Uint(_) = self.ty.kind() {
                    config::encode_unsigned_num_constraint()
                } else {
                    false
                };
                vec![vir::Predicate::new_primitive_value(
                    typ,
                    self.encoder.encode_value_field(self.ty),
                    bounds,
                    unsigned,
                )]
            }

            ty::TyKind::Ref(_, ref ty, _) => {
                vec![vir::Predicate::new_struct(
                    typ,
                    vec![self.encoder.encode_dereference_field(ty)?],
                )]
            },

            ty::TyKind::Tuple(elems) => {
                let fields = elems
                    .iter()
                    .enumerate()
                    .map(|(field_num, ty)| {
                        let field_name = format!("tuple_{}", field_num);
                        self.encoder.encode_raw_ref_field(field_name, ty.expect_ty())
                    })
                    .collect::<Result<_, _>>()?;
                vec![vir::Predicate::new_struct(typ, fields)]
            }

            ty::TyKind::Adt(adt_def, subst) if !adt_def.is_box() => {
                let num_variants = adt_def.variants.len();
                let tcx = self.encoder.env().tcx();
                if num_variants == 1 {
                    debug!("ADT {:?} has only one variant", adt_def);
                    let mut fields = vec![];
                    for field in &adt_def.variants[0usize.into()].fields {
                        let field_name = field.ident.to_string();
                        let field_ty = field.ty(tcx, subst);
                        fields.push(
                            self.encoder.encode_struct_field(
                                &field_name,
                                field_ty
                            )?
                        );
                    }
                    vec![vir::Predicate::new_struct(typ, fields)]
                } else {
                    debug!("ADT {:?} has {} variants", adt_def, num_variants);
                    let discriminant_field = self.encoder.encode_discriminant_field();
                    let this = vir::Predicate::construct_this(typ.clone());
                    let discriminant_loc =
                        vir::Expr::from(this.clone()).field(discriminant_field);
                    let discriminant_bounds =
                        compute_discriminant_bounds(adt_def, tcx, &discriminant_loc);

                    let discriminant_values = compute_discriminant_values(adt_def, tcx);
                    let variants: Vec<_> = adt_def
                        .variants
                        .iter()
                        .zip(discriminant_values)
                        .map(|(variant_def, variant_index)| {
                            let fields_res = variant_def
                                .fields
                                .iter()
                                .map(|field| {
                                    debug!("Encoding field {:?}", field);
                                    let field_name = &field.ident.as_str();
                                    let field_ty = field.ty(tcx, subst);
                                    self.encoder.encode_struct_field(field_name, field_ty)
                                })
                                .collect::<Result<_, _>>();
                            let variant_name = &variant_def.ident.as_str();
                            let guard = vir::Expr::eq_cmp(
                                discriminant_loc.clone().into(),
                                variant_index.into(),
                            );
                            let variant_typ = typ.clone().variant(variant_name);
                            fields_res.map(|fields| (
                                guard,
                                variant_name.to_string(),
                                vir::StructPredicate::new(variant_typ, fields),
                            ))
                        })
                        .collect::<Result<_, _>>()?;
                    for (_, name, _) in &variants {
                        self.encoder.encode_enum_variant_field(name);
                    }
                    let mut predicates: Vec<_> = variants
                        .iter()
                        .filter(|(_, _, predicate)| !predicate.has_empty_body())
                        .map(|(_, _, predicate)| vir::Predicate::Struct(predicate.clone()))
                        .collect();
                    let enum_predicate = vir::Predicate::new_enum(
                        this,
                        discriminant_loc,
                        discriminant_bounds,
                        variants,
                    );
                    predicates.push(enum_predicate);
                    predicates
                }
            }

            ty::TyKind::Adt(ref adt_def, ref _subst) if adt_def.is_box() => {
                let num_variants = adt_def.variants.len();
                assert_eq!(num_variants, 1);
                let field_ty = self.ty.boxed_ty();
                vec![vir::Predicate::new_struct(
                    typ,
                    vec![self.encoder.encode_dereference_field(field_ty)?],
                )]
            }

            ty::TyKind::Never => {
                // FIXME: This should be a predicate with the body `false`. See issue #38.
                vec![vir::Predicate::new_abstract(typ)]
            }

            ty::TyKind::Param(_) => {
                // special case: type parameters shall be encoded as *abstract* predicates
                vec![vir::Predicate::new_abstract(typ)]
            }

            ty::TyKind::Closure(_def_id, internal_substs) => {
                let closure_substs = internal_substs.as_closure();
                match closure_substs.tupled_upvars_ty().kind() {
                    ty::TyKind::Tuple(upvar_substs) => {
                        // TODO: this should encode the state of a closure, i.e.
                        // the "self" parameter passed into the implementation
                        // function generated for every closure. This should
                        // work using snapshots. For now, the "self" parameter
                        // is skipped in encoding.

                        // let field_name = "upvars".to_owned();
                        // let field = self.encoder.encode_raw_ref_field(field_name, cl_upvars);
                        // let pred = vir::Predicate::new_struct(typ.clone(), vec![field.clone()]);
                        let pred = vir::Predicate::new_struct(typ.clone(), vec![]);
                        // trace!("Encoded closure type {:?} as {:?} with field {:?}", typ, pred, field);
                        trace!("Encoded closure type {:?} as {:?}", typ, pred);
                        vec![pred]
                    }

                    _ => unreachable!()
                }
            }

            ref ty_variant => {
                debug!("Encoding of type '{:?}' is incomplete", ty_variant);
                vec![vir::Predicate::new_abstract(typ)]
            }
        })
    }

    pub fn encode_predicate_use(self) -> EncodingResult<String> {
        debug!("Encode type predicate name '{:?}'", self.ty);

        let result = match self.ty.kind() {
            ty::TyKind::Bool => "bool".to_string(),

            ty::TyKind::Int(ast::IntTy::I8) => "i8".to_string(),
            ty::TyKind::Int(ast::IntTy::I16) => "i16".to_string(),
            ty::TyKind::Int(ast::IntTy::I32) => "i32".to_string(),
            ty::TyKind::Int(ast::IntTy::I64) => "i64".to_string(),
            ty::TyKind::Int(ast::IntTy::I128) => "i128".to_string(),
            ty::TyKind::Int(ast::IntTy::Isize) => "isize".to_string(),

            ty::TyKind::Uint(ast::UintTy::U8) => "u8".to_string(),
            ty::TyKind::Uint(ast::UintTy::U16) => "u16".to_string(),
            ty::TyKind::Uint(ast::UintTy::U32) => "u32".to_string(),
            ty::TyKind::Uint(ast::UintTy::U64) => "u64".to_string(),
            ty::TyKind::Uint(ast::UintTy::U128) => "u128".to_string(),
            ty::TyKind::Uint(ast::UintTy::Usize) => "usize".to_string(),

            ty::TyKind::Char => "char".to_string(),

            ty::TyKind::RawPtr(ty::TypeAndMut { ref ty, .. }) => {
                format!("raw_ref${}", self.encoder.encode_type_predicate_use(ty)?)
            }
            ty::TyKind::Ref(_, ref ty, _) => {
                format!("ref${}", self.encoder.encode_type_predicate_use(ty)?)
            }

            ty::TyKind::Adt(adt_def, subst) => {
                let mut composed_name = vec![self.encoder.encode_item_name(adt_def.did)];
                composed_name.push("_beg_".to_string()); // makes generics "less fragile"
                let mut first = true;
                for kind in subst.iter() {
                    if first {
                        first = false
                    } else {
                        // makes generics "less fragile"
                        composed_name.push("_sep_".to_string());
                    }
                    if let ty::subst::GenericArgKind::Type(ty) = kind.unpack() {
                        composed_name.push(
                            self.encoder.encode_type_predicate_use(ty)?
                        )
                    }
                }
                composed_name.push("_end_".to_string()); // makes generics "less fragile"
                composed_name.join("$")
            }

            ty::TyKind::Tuple(elems) => {
                let elem_predicate_names: EncodingResult<Vec<_>> = elems
                    .iter()
                    .map(|ty| {
                        self.encoder.encode_type_predicate_use(ty.expect_ty())
                    })
                    .collect();
                format!(
                    "tuple{}${}",
                    elems.len(),
                    elem_predicate_names?.join("$")
                )
            }

            ty::TyKind::Never => "never".to_string(),

            ty::TyKind::Str => "str".to_string(),

            ty::TyKind::Array(elem_ty, size) => {
                let scalar_size = match size.val {
                    ty::ConstKind::Value(ref value) => {
                        value.try_to_bits(
                            rustc_target::abi::Size::from_bits(64)
                        ).unwrap()
                    },
                    ty::ConstKind::Unevaluated(def, ref substs, promoted) => {
                        let tcx = self.encoder.env().tcx();
                        let param_env = tcx.param_env(def.did);
                        tcx.const_eval_resolve(param_env, def, substs, promoted, None)
                            .ok()
                            .and_then(|const_value| const_value.try_to_bits(
                                rustc_target::abi::Size::from_bits(64)
                            ))
                            .unwrap()
                    }
                    x => unimplemented!("{:?}", x),
                };
                format!(
                    "array${}${}",
                    self.encoder.encode_type_predicate_use(elem_ty)?,
                    scalar_size
                )
            }

            ty::TyKind::Slice(array_ty) => {
                format!(
                    "slice${}",
                    self.encoder.encode_type_predicate_use(array_ty)?
                )
            }

            ty::TyKind::Closure(def_id, closure_subst) => {
                let subst_hash = {
                    let mut s = DefaultHasher::new();
                    closure_subst.hash(&mut s);
                    s.finish()
                };

                format!(
                    "closure${}_{}${}${}",
                    def_id.krate.as_u32(),
                    def_id.index.as_u32(),
                    closure_subst.len(),
                    subst_hash
                )
            }

            ty::TyKind::Param(param_ty) => {
                format!("__TYPARAM__${}$__", param_ty.name.as_str())
            }

            ty::TyKind::Projection(ty::ProjectionTy { item_def_id, .. }) => {
                self.encoder.encode_item_name(*item_def_id)
            }

            ty::TyKind::Dynamic(..) => {
                "unsupported$dynamic".to_string()
            }

            ty::TyKind::FnPtr(..) => {
                "unsupported$fnptr".to_string()
            }

            ty::TyKind::FnDef(..) => {
                "unsupported$fndef".to_string()
            }

            ty::TyKind::Foreign(..) => {
                "unsupported$foreign".to_string()
            }

            ref ty_variant => {
                "unsupported".to_string()
            }
        };
        Ok(result)
    }

    pub fn encode_invariant_def(self) -> EncodingResult<vir::Function> {
        debug!("[enter] encode_invariant_def({:?})", self.ty);

        let predicate_name = self.encoder.encode_type_predicate_use(self.ty)?;
        let self_local_var =
            vir::LocalVar::new("self", vir::Type::TypedRef(predicate_name.clone()));

        let invariant_name = self.encoder.encode_type_invariant_use(self.ty)?;

        let field_invariants = match self.ty.kind() {
            ty::TyKind::Ref(_, ref ty, _) => {
                let elem_field = self.encoder.encode_dereference_field(ty)?;
                let elem_loc = vir::Expr::from(self_local_var.clone()).field(elem_field);
                Some(vec![
                    self.encoder.encode_invariant_func_app(ty, elem_loc)?
                ])
            }

            ty::TyKind::Adt(ref adt_def, ref subst) if !adt_def.is_box() => {
                if self.is_supported_struct_type(adt_def, subst) {
                    let own_substs =
                        ty::List::identity_for_item(self.encoder.env().tcx(), adt_def.did);

                    {
                        // FIXME: this is a hack to support generics. See issue #187.
                        let mut tymap_stack = self.encoder.typaram_repl.borrow_mut();
                        let mut tymap = HashMap::new();

                        for (kind1, kind2) in own_substs.iter().zip(*subst) {
                            if let (
                                ty::subst::GenericArgKind::Type(ty1),
                                ty::subst::GenericArgKind::Type(ty2),
                            ) = (kind1.unpack(), kind2.unpack())
                            {
                                tymap.insert(ty1, ty2);
                            }
                        }
                        tymap_stack.push(tymap);
                    }

                    let mut exprs: Vec<vir::Expr> = vec![];
                    let num_variants = adt_def.variants.len();
                    let tcx = self.encoder.env().tcx();

                    let mut specs: Vec<typed::SpecificationSet> = Vec::new();
                    // FIXME: type invariants need to be collected separately
                    // in `SpecCollector`, and encoder should get a
                    // `get_struct_specs` method or similar.
                    // `get_procedure_specs` now only returns procedure specs,
                    // so the match below for `SpecSet::Struct` would never
                    // succeed.

                    //if let Some(spec) = self.encoder.get_procedure_specs(adt_def.did) {
                    //    specs.push(spec);
                    //}

                    let traits = self.encoder.env().get_traits_decls_for_type(&self.ty);
                    for trait_id in traits {
                        //if let Some(spec) = self.encoder.get_procedure_specs(trait_id) {
                        //    specs.push(spec);
                        //}
                    }

                    for spec in specs.into_iter() {
                        //let encoded_args = vec![vir::Expr::from(self_local_var.clone())];
                        let mut hacky_folder = HackyExprFolder {
                            saelf: self_local_var.clone(),
                        };

                        match spec {
                            typed::SpecificationSet::Struct(items) => {
                                for item in items {
                                    // let enc = encode_simple_spec_assertion(
                                    //     self.encoder,
                                    //     &[],
                                    //     &item.assertion
                                    // );
                                    let enc = unimplemented!(
                                        "TODO: type invariants need to be upgraded \
                                        to the new compiler version"
                                    );
                                    // OPEN TODO: hacky fix here to convert the closure var to "self"...
                                    let enc = hacky_folder.fold(enc);
                                    exprs.push(enc);
                                }
                            }
                            _ => unreachable!(),
                        }
                    }

                    // FIXME: this is a hack to support generics. See issue #187.
                    {
                        let mut tymap_stack = self.encoder.typaram_repl.borrow_mut();
                        tymap_stack.pop();
                    }

                    if num_variants == 0 {
                        debug!("ADT {:?} has no variant", adt_def);
                        // `false` here is currently unsound. See:
                        // * https://gitlab.inf.ethz.ch/OU-PMUELLER/prusti-dev/issues/158
                        // * https://gitlab.inf.ethz.ch/OU-PMUELLER/prusti-dev/issues/146
                        //exprs.push(false.into());
                    } else if num_variants == 1 && (adt_def.is_struct() || adt_def.is_union()) {
                        debug!("ADT {:?} has only one variant", adt_def);

                        for field in &adt_def.non_enum_variant().fields {
                            debug!("Encoding field {:?}", field);
                            let field_name = &field.ident.as_str();
                            let field_ty = field.ty(tcx, subst);
                            let elem_field = self.encoder.encode_struct_field(field_name, field_ty)?;
                            let elem_loc =
                                vir::Expr::from(self_local_var.clone()).field(elem_field);
                            exprs.push(
                                self.encoder.encode_invariant_func_app(
                                    field_ty,
                                    elem_loc
                                )?
                            );
                        }
                    } else {
                        debug!("ADT {:?} has {} variants", adt_def, num_variants);
                        // TODO: https://gitlab.inf.ethz.ch/OU-PMUELLER/prusti-dev/issues/201
                    }

                    Some(exprs)
                } else {
                    // TODO: https://gitlab.inf.ethz.ch/OU-PMUELLER/prusti-dev/issues/201
                    Some(vec![])
                }
            }

            // TODO
            _ => Some(vec![]),
        };

        let precondition = match self.ty.kind() {
            ty::TyKind::RawPtr(ty::TypeAndMut { ref ty, .. })
            | ty::TyKind::Ref(_, ref ty, _) => {
                // This is a reference, so we need to have it already unfolded.
                let elem_field = self.encoder.encode_dereference_field(ty)?;
                let elem_loc = vir::Expr::from(self_local_var.clone()).field(elem_field);
                vir::Expr::and(
                    vir::Expr::acc_permission(elem_loc.clone(), vir::PermAmount::Read),
                    vir::Expr::pred_permission(elem_loc, vir::PermAmount::Read).unwrap(),
                )
            }
            _ => vir::Expr::predicate_access_predicate(
                predicate_name,
                self_local_var.clone().into(),
                vir::PermAmount::Read,
            ),
        };

        let function = vir::Function {
            name: invariant_name,
            formal_args: vec![self_local_var],
            return_type: vir::Type::Bool,
            pres: vec![precondition],
            posts: Vec::new(),
            body: field_invariants.map(|invs| invs.into_iter().conjoin()),
        };

        // Add folding/unfolding
        let final_function = foldunfold::add_folding_unfolding_to_function(
            function,
            self.encoder.get_used_viper_predicates_map(),
        ).unwrap(); // TODO: generate a stub function in case of error
        debug!(
            "[exit] encode_invariant_def({:?}):\n{}",
            self.ty, final_function
        );
        Ok(final_function)
    }

    pub fn encode_invariant_use(self) -> EncodingResult<String> {
        debug!("Encode type invariant name '{:?}'", self.ty);
        Ok(format!("{}$inv", self.encode_predicate_use()?))
    }

    pub fn encode_tag_def(self) -> vir::Function {
        debug!("Encode type invariant '{:?}'", self.ty);

        //let pred_name = self.encoder.encode_type_tag_use(self.ty);
        //let self_local_var = vir::LocalVar::new("self", vir::Type::TypedRef(predicate_name.clone()));

        let tag_name = self.encoder.encode_type_tag_use(self.ty);

        let body = match self.ty.kind() {
            ty::TyKind::Param(_param_ty) => None,
            _ => Some((vir::Const::Int((self.ty as *const ty::TyS<'tcx>) as i64)).into()),
        };

        //let precondition = vir::Expr::PredicateAccessPredicate(
        //    predicate_name,
        //    vec![self_local_var.clone().into()],
        //    vir::PermAmount::Write,
        //);

        let function = vir::Function {
            name: tag_name,
            formal_args: Vec::new(),
            return_type: vir::Type::Int,
            pres: Vec::new(),
            posts: Vec::new(),
            body,
        };

        //// Add folding/unfolding
        //foldunfold::add_folding_unfolding_to_function(
        //    function,
        //    self.encoder.get_used_viper_predicates_map()
        //)
        function
    }

    pub fn encode_tag_use(self) -> EncodingResult<String> {
        debug!("Encode type tag name '{:?}'", self.ty);
        Ok(format!("{}$tag", self.encode_predicate_use()?))
    }
}

/// Compute the values that a discriminant can take.
pub fn compute_discriminant_values<'tcx>(adt_def: &'tcx ty::AdtDef, tcx: ty::TyCtxt<'tcx>) -> Vec<i128> {
    let mut discr_values: Vec<i128> = vec![];
    let size = ty::tls::with(|tcx| Integer::from_attr(&tcx, adt_def.repr.discr_type()).size());
    for (_variant_idx, discr) in adt_def.discriminants(tcx) {
        // Sign extend the raw representation to be an i128, to handle *signed* discriminants.
        // See also: https://github.com/rust-lang/rust/blob/b7ebc6b0c1ba3c27ebb17c0b496ece778ef11e18/compiler/rustc_middle/src/ty/util.rs#L35-L45
        discr_values.push(size.sign_extend(discr.val) as i128);
    }
    discr_values
}

/// Encode a disjunction that lists all possible discrimintant values.
pub fn compute_discriminant_bounds<'tcx>(
    adt_def: &'tcx ty::AdtDef,
    tcx: ty::TyCtxt<'tcx>,
    discriminant_loc: &vir::Expr,
) -> vir::Expr {
    /// Try to produce the minimal disjunction.
    fn build_discr_range_expr<T: Ord + PartialEq + Eq + Copy + Into<vir::Expr> + PlusOne>(
        discriminant_loc: &vir::Expr,
        discr_values: Vec<T>,
    ) -> vir::Expr {
        if discr_values.is_empty() {
            // A `false` here is unsound. See issues #38 and #158.
            return true.into();
        }
        range_extract(discr_values)
            .into_iter()
            .map(|(from, to)| {
                if from == to {
                    vir::Expr::eq_cmp(discriminant_loc.clone().into(), from.into())
                } else {
                    vir::Expr::and(
                        vir::Expr::le_cmp(from.into(), discriminant_loc.clone().into()),
                        vir::Expr::le_cmp(discriminant_loc.clone().into(), to.into()),
                    )
                }
            })
            .disjoin()
    }

    // Handle *signed* discriminats
    let discr_values = compute_discriminant_values(adt_def, tcx);
    build_discr_range_expr(discriminant_loc, discr_values)
}

struct HackyExprFolder {
    saelf: vir::LocalVar,
}

impl ExprFolder for HackyExprFolder {
    fn fold_local(&mut self, _var: vir::LocalVar, pos: vir::Position) -> vir::Expr {
        vir::Expr::Local(self.saelf.clone(), pos)
    }
}
