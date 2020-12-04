// © 2020, ETH Zurich
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use crate::encoder::{Encoder, snapshot_encoder};
use prusti_common::vir::{ExprFolder, compute_identifier, FallibleExprFolder};
use prusti_common::vir;
use crate::encoder::snapshot_encoder::Snapshot;
use crate::encoder::errors::PositionlessEncodingError;

pub struct SnapshotSpecPatcher<'p, 'v: 'p, 'tcx: 'v> {
    encoder: &'p Encoder<'v, 'tcx>,
}

impl<'p, 'v: 'p, 'tcx: 'v> SnapshotSpecPatcher<'p, 'v, 'tcx> {
    pub fn new(encoder: &'p Encoder<'v, 'tcx>) -> Self {
        SnapshotSpecPatcher {
            encoder,
        }
    }

    pub fn patch_spec(&self, spec: vir::Expr)
        -> Result<vir::Expr, PositionlessEncodingError>
    {
        PostSnapshotPatcher {
            encoder: self.encoder
        }.fallible_fold(spec)
    }
}

struct PostSnapshotPatcher<'p, 'v: 'p, 'tcx: 'v> {
    encoder: &'p Encoder<'v, 'tcx>,
}

impl<'p, 'v: 'p, 'tcx: 'v> FallibleExprFolder for PostSnapshotPatcher<'p, 'v, 'tcx> {
    type Error = PositionlessEncodingError;

    fn fallible_fold_func_app(
        &mut self,
        name: String,
        args: Vec<vir::Expr>,
        formal_args: Vec<vir::LocalVar>,
        return_type: vir::Type,
        pos: vir::Position,
    ) -> Result<vir::Expr, Self::Error> {
        let args : Vec<_> = args.into_iter()
            .map(|e| self.fallible_fold(e))
            .collect::<Result<_, _>>()?;
        // patch function calls that internally use snapshots
        Ok(if args.iter().any(|a| self.has_snap_type(a)) {
            match name.as_str() {
                // equalities, e.g. PartialEq::eq(__result, x), need to be patched as __result
                // is now a snapshot whereas x is not; the desired result is
                // __result == snapshot(x)
                snapshot_encoder::SNAPSHOT_EQUALS => {
                    self.patch_cmp_call(args, vir::BinOpKind::EqCmp)
                }
                snapshot_encoder::SNAPSHOT_NOT_EQUALS => {
                    self.patch_cmp_call(args, vir::BinOpKind::NeCmp)
                }
                _ => {
                    self.patch_func_app(name, args, formal_args, return_type, pos)?
                }
            }
        } else {
            self.default_fold_func_app(name, args, formal_args, return_type, pos)
        })
    }
}

impl<'p, 'v: 'p, 'tcx: 'v> PostSnapshotPatcher<'p, 'v, 'tcx> {
    fn patch_cmp_call(&self, args: Vec<vir::Expr>, cmp: vir::BinOpKind) -> vir::Expr {
        assert_eq!(args.len(), 2);
        let lhs_is_snap = self.has_snap_type(&args[0]);
        let rhs_is_snap = self.has_snap_type(&args[1]);

        let (lhs, rhs) = if (lhs_is_snap && rhs_is_snap)
            || (!lhs_is_snap && !rhs_is_snap) {
            (
                args[0].clone(),
                args[1].clone()
            )
        } else if lhs_is_snap /* && !rhs_is_snap */ {
            (
                args[0].clone(),
                self.get_snapshot(&args[0]).snap_call(args[1].clone())
            )
        } else /* rhs_is_snap && !lhs_is_snap */ {
            (
                self.get_snapshot(&args[1]).snap_call(args[0].clone()),
                args[1].clone()
            )
        };

        vir::Expr::BinOp(
            cmp,
            box lhs,
            box rhs,
            vir::Position::default()
        )
    }

    fn has_snap_type(&self, expr: &vir::Expr) -> bool {
        if expr.is_place() || expr.is_call() {
            match expr.get_type() {
                vir::Type::Domain(_) => true,
                _ => false,
            }
        } else {
            false
        }
    }

    fn get_snapshot(&self, expr: &vir::Expr) -> Box<Snapshot> {
        match expr.get_type() {
            vir::Type::Domain(snapshot_name) => {
                self.encoder.get_snapshot(snapshot_name.to_string())
            },
            _ => unreachable!(),
        }
    }

    fn patch_func_app_with_mirror(
        &mut self,
        mirror_func: vir::DomainFunc,
        args: Vec<vir::Expr>,
        pos: vir::Position
    ) -> Result<vir::Expr, PositionlessEncodingError> {
        let patched_args = args
            .into_iter()
            .map(|a|
                if a.is_place() { // for constants
                    match a.get_type() {
                        vir::Type::TypedRef(predicate_name) => {
                            self.encoder
                                .encode_snapshot_use(
                                    predicate_name.to_string()
                                )
                                .map(|snapshot|
                                    if snapshot.is_defined() {
                                        snapshot.snap_call(a)
                                    } else {
                                        a
                                    }
                                )
                        }
                        _ => Ok(a),
                    }
                } else {
                    Ok(a)
                }
            ).collect::<Result<_, _>>()?;

        Ok(vir::Expr::DomainFuncApp(
            mirror_func,
            patched_args,
            pos,
        ))
    }

    fn patch_func_app(
        &mut self,
        name: String,
        args: Vec<vir::Expr>,
        formal_args: Vec<vir::LocalVar>,
        return_type: vir::Type,
        pos: vir::Position,
    ) -> Result<vir::Expr, PositionlessEncodingError> {
        // we need to rectify cases in which there is a mismatch between the
        // functions formal arguments (which do not involve snapshots)
        // and its actual arguments (which may involve snapshots)
        let found_mismatch = formal_args
            .iter()
            .zip(args.iter())
            .any(|(f, a)| f.typ != *a.get_type());

        Ok(if found_mismatch {
            let encoded_mirror_func = self.encoder.encode_pure_snapshot_mirror(
                compute_identifier(&name, &formal_args, &return_type),
                &formal_args,
                &return_type
            )?;
            if let Some(mirror_func) = encoded_mirror_func {
                self.patch_func_app_with_mirror(mirror_func, args, pos)?
            } else {
                self.default_fold_func_app(name, args, formal_args, return_type, pos)
            }
        } else {
            self.default_fold_func_app(name, args, formal_args, return_type, pos)
        })
    }

    fn default_fold_func_app(
        &mut self,
        name: String,
        args: Vec<vir::Expr>,
        formal_args: Vec<vir::LocalVar>,
        return_type: vir::Type,
        pos: vir::Position,
    ) -> vir::Expr {
        vir::Expr::FuncApp(
            name,
            args,
            formal_args,
            return_type,
            pos
        )
    }
}