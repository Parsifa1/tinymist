//! Infer more than the principal type of some expression.

use hashbrown::HashSet;
use tinymist_derive::BindTyCtx;

use super::{prelude::*, DynTypeBounds, ParamAttrs, ParamTy, SharedContext};
use super::{
    ArgsTy, Sig, SigChecker, SigShape, SigSurfaceKind, SigTy, Ty, TyCtx, TyCtxMut, TypeBounds,
    TypeScheme, TypeVar,
};
use crate::syntax::{get_check_target, get_check_target_by_context, CheckTarget, ParamTarget};
use crate::ty::BuiltinTy;

/// With given type information, check the type of a literal expression again by
/// touching the possible related nodes.
pub(crate) fn post_type_check(
    ctx: Arc<SharedContext>,
    info: &TypeScheme,
    node: LinkedNode,
) -> Option<Ty> {
    let mut checker = PostTypeChecker::new(ctx, info);
    let res = checker.check(&node);
    checker.simplify(&res?)
}

#[derive(Default)]
struct SignatureReceiver {
    lbs_dedup: HashSet<Ty>,
    ubs_dedup: HashSet<Ty>,
    bounds: TypeBounds,
}

impl SignatureReceiver {
    fn insert(&mut self, ty: Ty, pol: bool) {
        crate::log_debug_ct!("post check receive: {ty:?}");
        if !pol {
            if self.lbs_dedup.insert(ty.clone()) {
                self.bounds.lbs.push(ty);
            }
        } else if self.ubs_dedup.insert(ty.clone()) {
            self.bounds.ubs.push(ty);
        }
    }

    fn finalize(self) -> Ty {
        Ty::Let(self.bounds.into())
    }
}

fn check_signature<'a>(
    receiver: &'a mut SignatureReceiver,
    target: &'a ParamTarget,
) -> impl FnMut(&mut PostTypeChecker, Sig, &[Interned<ArgsTy>], bool) -> Option<()> + 'a {
    move |worker, sig, args, pol| {
        let (sig, _is_partialize) = match sig {
            Sig::Partialize(sig) => (*sig, true),
            sig => (sig, false),
        };

        let SigShape { sig: sig_ins, .. } = sig.shape(worker)?;

        match &target {
            ParamTarget::Named(n) => {
                let ident = n.cast::<ast::Ident>()?;
                let ty = sig_ins.named(&ident.into())?;
                receiver.insert(ty.clone(), !pol);

                Some(())
            }
            ParamTarget::Positional {
                // todo: spreads
                spreads: _,
                positional,
                is_spread,
            } => {
                if *is_spread {
                    return None;
                }

                // truncate args
                let bound_pos = args
                    .iter()
                    .map(|args| args.positional_params().len())
                    .sum::<usize>();
                if let Some(nth) = sig_ins.pos_or_rest(bound_pos + positional) {
                    receiver.insert(nth, !pol);
                }

                // names
                for (name, _) in sig_ins.named_params() {
                    // todo: reduce fields, fields ty
                    let field = ParamTy::new_untyped(name.clone(), ParamAttrs::named());
                    receiver.insert(Ty::Param(field), !pol);
                }

                Some(())
            }
        }
    }
}

pub(crate) struct PostTypeChecker<'a> {
    ctx: Arc<SharedContext>,
    pub info: &'a TypeScheme,
    checked: HashMap<Span, Option<Ty>>,
    locals: TypeScheme,
}

impl TyCtx for PostTypeChecker<'_> {
    fn global_bounds(&self, var: &Interned<TypeVar>, pol: bool) -> Option<DynTypeBounds> {
        self.info.global_bounds(var, pol)
    }

    fn local_bind_of(&self, var: &Interned<TypeVar>) -> Option<Ty> {
        self.locals.local_bind_of(var)
    }
}

impl TyCtxMut for PostTypeChecker<'_> {
    type Snap = <TypeScheme as TyCtxMut>::Snap;

    fn start_scope(&mut self) -> Self::Snap {
        self.locals.start_scope()
    }

    fn end_scope(&mut self, snap: Self::Snap) {
        self.locals.end_scope(snap)
    }

    fn bind_local(&mut self, var: &Interned<TypeVar>, ty: Ty) {
        self.locals.bind_local(var, ty);
    }

    fn type_of_func(&mut self, func: &Func) -> Option<Interned<SigTy>> {
        Some(self.ctx.type_of_func(func.clone()).type_sig())
    }

    fn type_of_value(&mut self, val: &Value) -> Ty {
        self.ctx.type_of_value(val)
    }

    fn check_module_item(&mut self, _module: TypstFileId, _key: &StrRef) -> Option<Ty> {
        None
    }
}

impl<'a> PostTypeChecker<'a> {
    pub fn new(ctx: Arc<SharedContext>, info: &'a TypeScheme) -> Self {
        Self {
            ctx,
            info,
            checked: HashMap::new(),
            locals: TypeScheme::default(),
        }
    }

    fn check(&mut self, node: &LinkedNode) -> Option<Ty> {
        let span = node.span();
        if let Some(ty) = self.checked.get(&span) {
            return ty.clone();
        }
        // loop detection
        self.checked.insert(span, None);

        let ty = self.check_(node);
        self.checked.insert(span, ty.clone());
        ty
    }

    fn simplify(&mut self, ty: &Ty) -> Option<Ty> {
        Some(self.info.simplify(ty.clone(), false))
    }

    fn check_(&mut self, node: &LinkedNode) -> Option<Ty> {
        let context = node.parent()?;
        crate::log_debug_ct!("post check: {:?}::{:?}", context.kind(), node.kind());

        let context_ty = self.check_context(context, node);
        let self_ty = if !matches!(node.kind(), SyntaxKind::Label | SyntaxKind::Ref) {
            self.info.type_of_span(node.span())
        } else {
            None
        };

        let contextual_self_ty = self.check_target(get_check_target(node.clone()), context_ty);
        crate::log_debug_ct!(
            "post check(res): {:?}::{:?} -> {self_ty:?}, {contextual_self_ty:?}",
            context.kind(),
            node.kind(),
        );

        Ty::union(self_ty, contextual_self_ty)
    }

    fn check_or(&mut self, node: &LinkedNode, ty: Option<Ty>) -> Option<Ty> {
        Ty::union(self.check(node), ty)
    }

    fn check_target(&mut self, node: Option<CheckTarget>, context_ty: Option<Ty>) -> Option<Ty> {
        let Some(node) = node else {
            return context_ty;
        };
        crate::log_debug_ct!("post check target: {node:?}");

        match &node {
            CheckTarget::Param {
                callee,
                args: _,
                target,
                is_set,
            } => {
                let callee = self.check_or(callee, context_ty)?;
                crate::log_debug_ct!(
                    "post check call target: ({callee:?})::{target:?} is_set: {is_set}"
                );

                let sig = self.ctx.sig_of_type(self.info, callee)?;
                crate::log_debug_ct!("post check call sig: {target:?} {sig:?}");
                let mut resp = SignatureReceiver::default();

                match target {
                    ParamTarget::Named(n) => {
                        let ident = n.cast::<ast::Ident>()?.into();
                        let ty = sig.primary().get_named(&ident)?;
                        // todo: losing docs
                        resp.insert(ty.ty.clone(), false);
                    }
                    ParamTarget::Positional {
                        // todo: spreads
                        spreads: _,
                        positional,
                        is_spread,
                    } => {
                        if *is_spread {
                            return None;
                        }

                        // truncate args
                        let c = sig.param_shift();
                        let nth = sig
                            .primary()
                            .get_pos(c + positional)
                            .or_else(|| sig.primary().rest());
                        if let Some(nth) = nth {
                            resp.insert(Ty::Param(nth.clone()), false);
                        }

                        // names
                        for field in sig.primary().named() {
                            if *is_set && !field.attrs.settable {
                                continue;
                            }

                            resp.insert(Ty::Param(field.clone()), false);
                        }
                    }
                }

                crate::log_debug_ct!("post check target iterated: {:?}", resp.bounds);
                Some(resp.finalize())
            }
            CheckTarget::Element { container, target } => {
                let container_ty = self.check_or(container, context_ty)?;
                crate::log_debug_ct!("post check element target: ({container_ty:?})::{target:?}");

                let mut resp = SignatureReceiver::default();

                self.check_element_of(
                    &container_ty,
                    false,
                    container,
                    &mut check_signature(&mut resp, target),
                );

                crate::log_debug_ct!("post check target iterated: {:?}", resp.bounds);
                Some(resp.finalize())
            }
            CheckTarget::Paren {
                container,
                is_before,
            } => {
                let container_ty = self.check_or(container, context_ty)?;
                crate::log_debug_ct!("post check paren target: {container_ty:?}::{is_before:?}");

                let mut resp = SignatureReceiver::default();
                // todo: this is legal, but it makes it sometimes complete itself.
                // e.g. completing `""` on `let x = ("|")`
                resp.bounds.lbs.push(container_ty.clone());

                let target = ParamTarget::positional_from_before(true);
                self.check_element_of(
                    &container_ty,
                    false,
                    container,
                    &mut check_signature(&mut resp, &target),
                );

                crate::log_debug_ct!("post check target iterated: {:?}", resp.bounds);
                Some(resp.finalize())
            }
            CheckTarget::ImportPath(..) | CheckTarget::IncludePath(..) => Some(Ty::Builtin(
                BuiltinTy::Path(crate::ty::PathPreference::Source {
                    allow_package: true,
                }),
            )),
            CheckTarget::LabelError(target)
            | CheckTarget::Label(target)
            | CheckTarget::Normal(target) => {
                let label_ty = matches!(node, CheckTarget::LabelError(_))
                    .then_some(Ty::Builtin(BuiltinTy::Label));
                let ty = self.check_or(target, context_ty);
                crate::log_debug_ct!("post check target normal: {ty:?} {label_ty:?}");
                ty.or(label_ty)
            }
        }
    }

    fn check_context(&mut self, context: &LinkedNode, node: &LinkedNode) -> Option<Ty> {
        match context.kind() {
            SyntaxKind::LetBinding => {
                let p = context.cast::<ast::LetBinding>()?;
                let exp = p.init()?;
                if exp.span() != node.span() {
                    return None;
                }

                match p.kind() {
                    ast::LetBindingKind::Closure(_c) => None,
                    ast::LetBindingKind::Normal(pattern) => {
                        self.destruct_let(pattern, node.clone())
                    }
                }
            }
            SyntaxKind::Args => self.check_target(
                // todo: not well behaved
                get_check_target_by_context(context.clone(), node.clone()),
                None,
            ),
            // todo: constraint node
            SyntaxKind::Named => self.check_target(get_check_target(context.clone()), None),
            _ => None,
        }
    }

    fn destruct_let(&mut self, pattern: ast::Pattern, node: LinkedNode) -> Option<Ty> {
        match pattern {
            ast::Pattern::Placeholder(_) => None,
            ast::Pattern::Normal(n) => {
                let ast::Expr::Ident(ident) = n else {
                    return None;
                };
                self.info.type_of_span(ident.span())
            }
            ast::Pattern::Parenthesized(p) => {
                self.destruct_let(p.expr().to_untyped().cast()?, node)
            }
            // todo: pattern matching
            ast::Pattern::Destructuring(_d) => {
                let _ = node;
                None
            }
        }
    }

    fn check_element_of<T>(&mut self, ty: &Ty, pol: bool, context: &LinkedNode, checker: &mut T)
    where
        T: PostSigChecker,
    {
        let mut checker = PostSigCheckWorker(self, checker);
        ty.sig_surface(pol, sig_context_of(context), &mut checker)
    }
}

trait PostSigChecker {
    fn check(
        &mut self,
        checker: &mut PostTypeChecker,
        sig: Sig,
        args: &[Interned<ArgsTy>],
        pol: bool,
    ) -> Option<()>;
}

impl<T> PostSigChecker for T
where
    T: FnMut(&mut PostTypeChecker, Sig, &[Interned<ArgsTy>], bool) -> Option<()>,
{
    fn check(
        &mut self,
        checker: &mut PostTypeChecker,
        sig: Sig,
        args: &[Interned<ArgsTy>],
        pol: bool,
    ) -> Option<()> {
        self(checker, sig, args, pol)
    }
}

#[derive(BindTyCtx)]
#[bind(0)]
struct PostSigCheckWorker<'x, 'a, T>(&'x mut PostTypeChecker<'a>, &'x mut T);

impl<T: PostSigChecker> SigChecker for PostSigCheckWorker<'_, '_, T> {
    fn check(
        &mut self,
        sig: Sig,
        args: &mut crate::analysis::SigCheckContext,
        pol: bool,
    ) -> Option<()> {
        self.1.check(self.0, sig, &args.args, pol)
    }
}

fn sig_context_of(context: &LinkedNode) -> SigSurfaceKind {
    match context.kind() {
        SyntaxKind::Parenthesized => SigSurfaceKind::ArrayOrDict,
        SyntaxKind::Array => {
            let c = context.cast::<ast::Array>();
            if c.is_some_and(|e| e.items().next().is_some()) {
                SigSurfaceKind::Array
            } else {
                SigSurfaceKind::ArrayOrDict
            }
        }
        SyntaxKind::Dict => SigSurfaceKind::Dict,
        _ => SigSurfaceKind::Array,
    }
}
