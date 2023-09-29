// Copyright (c) The Diem Core Contributors
// Copyright (c) The Move Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{
    diag,
    expansion::ast::{self as E, AbilitySet, Fields, ModuleIdent},
    hlir::ast::{self as H, Block, MoveOpAnnotation},
    naming::ast as N,
    parser::ast::{BinOp, BinOp_, ConstantName, Field, FunctionName, StructName},
    shared::{unique_map::UniqueMap, *},
    typing::ast as T,
    FullyCompiledProgram,
};
use move_ir_types::location::*;
use move_symbol_pool::Symbol;
use once_cell::sync::Lazy;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    convert::TryInto,
};

//**************************************************************************************************
// Vars
//**************************************************************************************************

const NEW_NAME_DELIM: &str = "#";

fn translate_var(sp!(loc, v_): N::Var) -> H::Var {
    let N::Var_ {
        name,
        id: depth,
        color,
    } = v_;
    let s = format!(
        "{}{}{}{}{}",
        name, NEW_NAME_DELIM, depth, NEW_NAME_DELIM, color
    )
    .into();
    H::Var(sp(loc, s))
}

const TEMP_PREFIX: &str = "%";
static TEMP_PREFIX_SYMBOL: Lazy<Symbol> = Lazy::new(|| TEMP_PREFIX.into());

fn new_temp_name(context: &mut Context) -> Symbol {
    format!(
        "{}{}{}",
        *TEMP_PREFIX_SYMBOL,
        NEW_NAME_DELIM,
        context.counter_next()
    )
    .into()
}

pub fn is_temp_name(s: Symbol) -> bool {
    s.starts_with(TEMP_PREFIX)
}

pub enum DisplayVar {
    Orig(String),
    Tmp,
}

pub fn display_var(s: Symbol) -> DisplayVar {
    if is_temp_name(s) {
        DisplayVar::Tmp
    } else {
        let mut orig = s.as_str().to_string();
        if let Some(i) = orig.find(NEW_NAME_DELIM) {
            orig.truncate(i)
        }
        DisplayVar::Orig(orig)
    }
}

//**************************************************************************************************
// Context
//**************************************************************************************************

struct Context<'env> {
    env: &'env mut CompilationEnv,
    structs: UniqueMap<ModuleIdent, UniqueMap<StructName, UniqueMap<Field, usize>>>,
    function_locals: UniqueMap<H::Var, H::SingleType>,
    signature: Option<H::FunctionSignature>,
    tmp_counter: usize,
    named_block_binders: UniqueMap<H::Var, Vec<H::LValue>>,
    named_block_types: UniqueMap<H::Var, H::Type>,
    /// collects all struct fields used in the current module
    pub used_fields: BTreeMap<Symbol, BTreeSet<Symbol>>,
}

impl<'env> Context<'env> {
    pub fn new(
        env: &'env mut CompilationEnv,
        pre_compiled_lib_opt: Option<&FullyCompiledProgram>,
        prog: &T::Program,
    ) -> Self {
        fn add_struct_fields(
            structs: &mut UniqueMap<ModuleIdent, UniqueMap<StructName, UniqueMap<Field, usize>>>,
            mident: ModuleIdent,
            struct_defs: &UniqueMap<StructName, N::StructDefinition>,
        ) {
            let mut cur_structs = UniqueMap::new();
            for (sname, sdef) in struct_defs.key_cloned_iter() {
                let mut fields = UniqueMap::new();
                let field_map = match &sdef.fields {
                    N::StructFields::Native(_) => continue,
                    N::StructFields::Defined(m) => m,
                };
                for (field, (idx, _)) in field_map.key_cloned_iter() {
                    fields.add(field, *idx).unwrap();
                }
                cur_structs.add(sname, fields).unwrap();
            }
            structs.remove(&mident);
            structs.add(mident, cur_structs).unwrap();
        }

        let mut structs = UniqueMap::new();
        if let Some(pre_compiled_lib) = pre_compiled_lib_opt {
            for (mident, mdef) in pre_compiled_lib.typing.modules.key_cloned_iter() {
                add_struct_fields(&mut structs, mident, &mdef.structs)
            }
        }
        for (mident, mdef) in prog.modules.key_cloned_iter() {
            add_struct_fields(&mut structs, mident, &mdef.structs)
        }
        Context {
            env,
            structs,
            function_locals: UniqueMap::new(),
            signature: None,
            tmp_counter: 0,
            used_fields: BTreeMap::new(),
            named_block_binders: UniqueMap::new(),
            named_block_types: UniqueMap::new(),
        }
    }

    pub fn has_empty_locals(&self) -> bool {
        self.function_locals.is_empty()
    }

    pub fn extract_function_locals(&mut self) -> UniqueMap<H::Var, H::SingleType> {
        self.tmp_counter = 0;
        std::mem::replace(&mut self.function_locals, UniqueMap::new())
    }

    pub fn new_temp(&mut self, loc: Loc, t: H::SingleType) -> H::Var {
        let new_var = H::Var(sp(loc, new_temp_name(self)));
        self.function_locals.add(new_var, t).unwrap();

        new_var
    }

    pub fn bind_local(&mut self, v: N::Var, t: H::SingleType) {
        let symbol = translate_var(v);
        self.function_locals.add(symbol, t).unwrap();
    }

    pub fn record_named_block_binders(&mut self, block_name: H::Var, binders: Vec<H::LValue>) {
        self.named_block_binders
            .add(block_name, binders)
            .expect("ICE reused block name");
    }

    pub fn record_named_block_type(&mut self, block_name: H::Var, ty: H::Type) {
        self.named_block_types
            .add(block_name, ty)
            .expect("ICE reused block name");
    }

    pub fn lookup_named_block_binders(&mut self, block_name: &H::Var) -> Vec<H::LValue> {
        self.named_block_binders
            .get(block_name)
            .expect("ICE named block with no binders")
            .clone()
    }

    pub fn lookup_named_block_type(&mut self, block_name: &H::Var) -> Option<H::Type> {
        self.named_block_types.get(block_name).cloned()
    }

    pub fn fields(
        &self,
        module: &ModuleIdent,
        struct_name: &StructName,
    ) -> Option<&UniqueMap<Field, usize>> {
        let fields = self
            .structs
            .get(module)
            .and_then(|structs| structs.get(struct_name));
        // if fields are none, the struct must be defined in another module,
        // in that case, there should be errors
        assert!(fields.is_some() || self.env.has_errors());
        fields
    }

    fn counter_next(&mut self) -> usize {
        self.tmp_counter += 1;
        self.tmp_counter
    }

    fn exit_function(&mut self) {
        self.signature = None;
        self.named_block_binders = UniqueMap::new();
        self.named_block_types = UniqueMap::new();
    }
}

//**************************************************************************************************
// Entry
//**************************************************************************************************

pub fn program(
    compilation_env: &mut CompilationEnv,
    pre_compiled_lib: Option<&FullyCompiledProgram>,
    prog: T::Program,
) -> H::Program {
    let mut context = Context::new(compilation_env, pre_compiled_lib, &prog);
    let T::Program {
        modules: tmodules,
        scripts: tscripts,
    } = prog;
    let modules = modules(&mut context, tmodules);
    let scripts = scripts(&mut context, tscripts);

    H::Program { modules, scripts }
}

fn modules(
    context: &mut Context,
    modules: UniqueMap<ModuleIdent, T::ModuleDefinition>,
) -> UniqueMap<ModuleIdent, H::ModuleDefinition> {
    let hlir_modules = modules
        .into_iter()
        .map(|(mname, m)| module(context, mname, m));
    UniqueMap::maybe_from_iter(hlir_modules).unwrap()
}

fn module(
    context: &mut Context,
    module_ident: ModuleIdent,
    mdef: T::ModuleDefinition,
) -> (ModuleIdent, H::ModuleDefinition) {
    let T::ModuleDefinition {
        warning_filter,
        package_name,
        attributes,
        is_source_module,
        dependency_order,
        friends,
        structs: tstructs,
        functions: tfunctions,
        constants: tconstants,
    } = mdef;
    context.env.add_warning_filter_scope(warning_filter.clone());
    let structs = tstructs.map(|name, s| struct_def(context, name, s));

    let constants = tconstants.map(|name, c| constant(context, name, c));
    let functions = tfunctions.map(|name, f| function(context, name, f));

    gen_unused_warnings(context, is_source_module, &structs);

    context.env.pop_warning_filter_scope();
    (
        module_ident,
        H::ModuleDefinition {
            warning_filter,
            package_name,
            attributes,
            is_source_module,
            dependency_order,
            friends,
            structs,
            constants,
            functions,
        },
    )
}

fn scripts(
    context: &mut Context,
    tscripts: BTreeMap<Symbol, T::Script>,
) -> BTreeMap<Symbol, H::Script> {
    tscripts
        .into_iter()
        .map(|(n, s)| (n, script(context, s)))
        .collect()
}

fn script(context: &mut Context, tscript: T::Script) -> H::Script {
    let T::Script {
        warning_filter,
        package_name,
        attributes,
        loc,
        constants: tconstants,
        function_name,
        function: tfunction,
    } = tscript;
    context.env.add_warning_filter_scope(warning_filter.clone());
    let constants = tconstants.map(|name, c| constant(context, name, c));
    let function = function(context, function_name, tfunction);
    context.env.pop_warning_filter_scope();
    H::Script {
        warning_filter,
        package_name,
        attributes,
        loc,
        constants,
        function_name,
        function,
    }
}

//**************************************************************************************************
// Functions
//**************************************************************************************************

fn function(context: &mut Context, _name: FunctionName, f: T::Function) -> H::Function {
    assert!(context.has_empty_locals());
    assert!(context.tmp_counter == 0);
    let T::Function {
        warning_filter,
        index,
        attributes,
        visibility: evisibility,
        entry,
        signature,
        acquires,
        body,
    } = f;
    context.env.add_warning_filter_scope(warning_filter.clone());
    if DEBUG_PRINT {
        println!("Processing {:?}", _name);
    }
    let signature = function_signature(context, signature);
    let body = function_body(context, &signature, body);
    context.env.pop_warning_filter_scope();
    H::Function {
        warning_filter,
        index,
        attributes,
        visibility: visibility(evisibility),
        entry,
        signature,
        acquires,
        body,
    }
}

fn function_signature(context: &mut Context, sig: N::FunctionSignature) -> H::FunctionSignature {
    let type_parameters = sig.type_parameters;
    let parameters = sig
        .parameters
        .into_iter()
        .map(|(v, tty)| {
            let ty = single_type(context, tty);
            context.bind_local(v, ty.clone());
            (translate_var(v), ty)
        })
        .collect();
    let return_type = type_(context, sig.return_type);
    H::FunctionSignature {
        type_parameters,
        parameters,
        return_type,
    }
}

fn function_body(
    context: &mut Context,
    sig: &H::FunctionSignature,
    sp!(loc, tb_): T::FunctionBody,
) -> H::FunctionBody {
    use H::FunctionBody_ as HB;
    use T::FunctionBody_ as TB;
    let b_ = match tb_ {
        TB::Native => {
            context.extract_function_locals();
            HB::Native
        }
        TB::Defined(seq) => {
            let (locals, body) = function_body_defined(context, sig, loc, seq);
            HB::Defined { locals, body }
        }
    };
    sp(loc, b_)
}

const DEBUG_PRINT: bool = false;

fn function_body_defined(
    context: &mut Context,
    signature: &H::FunctionSignature,
    loc: Loc,
    seq: T::Sequence,
) -> (UniqueMap<H::Var, H::SingleType>, Block) {
    context.signature = Some(signature.clone());

    if DEBUG_PRINT {
        println!("--------------------------------------------------");
        crate::shared::ast_debug::print_verbose(&seq);
    }
    let (mut body, final_value) = { body(context, Some(&signature.return_type), loc, seq) };
    if let Some(ret_exp) = final_value {
        let ret_loc = ret_exp.exp.loc;
        let ret_command = H::Command_::Return {
            from_user: false,
            exp: ret_exp,
        };
        body.push_back(make_command(ret_loc, ret_command));
    }

    let locals = context.extract_function_locals();
    // check_trailing_unit(context, &mut body);
    if DEBUG_PRINT {
        println!("--------------------");
        crate::shared::ast_debug::print_verbose(&body);
        println!("--------------------------------------------------");
    }
    context.exit_function();
    (locals, body)
}

fn visibility(evisibility: E::Visibility) -> H::Visibility {
    match evisibility {
        E::Visibility::Internal => H::Visibility::Internal,
        E::Visibility::Friend(loc) => H::Visibility::Friend(loc),
        // We added any friends we needed during typing, so we convert this over.
        E::Visibility::Package(loc) => H::Visibility::Friend(loc),
        E::Visibility::Public(loc) => H::Visibility::Public(loc),
    }
}

//**************************************************************************************************
// Constants
//**************************************************************************************************

fn constant(context: &mut Context, _name: ConstantName, cdef: T::Constant) -> H::Constant {
    let T::Constant {
        warning_filter,
        index,
        attributes,
        loc,
        signature: tsignature,
        value: tvalue,
    } = cdef;
    context.env.add_warning_filter_scope(warning_filter.clone());
    let signature = base_type(context, tsignature);
    let eloc = tvalue.exp.loc;
    let tseq = {
        let mut v = T::Sequence::new();
        v.push_back(sp(eloc, T::SequenceItem_::Seq(Box::new(tvalue))));
        v
    };
    let function_signature = H::FunctionSignature {
        type_parameters: vec![],
        parameters: vec![],
        return_type: H::Type_::base(signature.clone()),
    };
    let (locals, body) = function_body_defined(context, &function_signature, loc, tseq);
    context.env.pop_warning_filter_scope();
    H::Constant {
        warning_filter,
        index,
        attributes,
        loc,
        signature,
        value: (locals, body),
    }
}

//**************************************************************************************************
// Structs
//**************************************************************************************************

fn struct_def(
    context: &mut Context,
    _name: StructName,
    sdef: N::StructDefinition,
) -> H::StructDefinition {
    let N::StructDefinition {
        warning_filter,
        index,
        attributes,
        abilities,
        type_parameters,
        fields,
    } = sdef;
    context.env.add_warning_filter_scope(warning_filter.clone());
    let fields = struct_fields(context, fields);
    context.env.pop_warning_filter_scope();
    H::StructDefinition {
        warning_filter,
        index,
        attributes,
        abilities,
        type_parameters,
        fields,
    }
}

fn struct_fields(context: &mut Context, tfields: N::StructFields) -> H::StructFields {
    let tfields_map = match tfields {
        N::StructFields::Native(loc) => return H::StructFields::Native(loc),
        N::StructFields::Defined(m) => m,
    };
    let mut indexed_fields = tfields_map
        .into_iter()
        .map(|(f, (idx, t))| (idx, (f, base_type(context, t))))
        .collect::<Vec<_>>();
    indexed_fields.sort_by(|(idx1, _), (idx2, _)| idx1.cmp(idx2));
    H::StructFields::Defined(indexed_fields.into_iter().map(|(_, f_ty)| f_ty).collect())
}

//**************************************************************************************************
// Types
//**************************************************************************************************

fn type_name(_context: &Context, sp!(loc, ntn_): N::TypeName) -> H::TypeName {
    use H::TypeName_ as HT;
    use N::TypeName_ as NT;
    let tn_ = match ntn_ {
        NT::Multiple(_) => panic!(
            "ICE type constraints failed {}:{}-{}",
            loc.file_hash(),
            loc.start(),
            loc.end()
        ),
        NT::Builtin(bt) => HT::Builtin(bt),
        NT::ModuleType(m, s) => HT::ModuleType(m, s),
    };
    sp(loc, tn_)
}

fn base_types<R: std::iter::FromIterator<H::BaseType>>(
    context: &Context,
    tys: impl IntoIterator<Item = N::Type>,
) -> R {
    tys.into_iter().map(|t| base_type(context, t)).collect()
}

fn base_type(context: &Context, sp!(loc, nb_): N::Type) -> H::BaseType {
    use H::BaseType_ as HB;
    use N::Type_ as NT;
    let b_ = match nb_ {
        NT::Var(_) => panic!(
            "ICE tvar not expanded: {}:{}-{}",
            loc.file_hash(),
            loc.start(),
            loc.end()
        ),
        NT::Apply(None, n, tys) => {
            crate::shared::ast_debug::print_verbose(&NT::Apply(None, n, tys));
            panic!("ICE kind not expanded: {:#?}", loc)
        }
        NT::Apply(Some(k), n, nbs) => HB::Apply(k, type_name(context, n), base_types(context, nbs)),
        NT::Param(tp) => HB::Param(tp),
        NT::UnresolvedError => HB::UnresolvedError,
        NT::Anything => HB::Unreachable,
        NT::Ref(_, _) | NT::Unit => {
            panic!(
                "ICE type constraints failed {}:{}-{}",
                loc.file_hash(),
                loc.start(),
                loc.end()
            )
        }
    };
    sp(loc, b_)
}

fn expected_types(context: &Context, loc: Loc, nss: Vec<Option<N::Type>>) -> H::Type {
    let any = || {
        sp(
            loc,
            H::SingleType_::Base(sp(loc, H::BaseType_::UnresolvedError)),
        )
    };
    let ss = nss
        .into_iter()
        .map(|sopt| sopt.map(|s| single_type(context, s)).unwrap_or_else(any))
        .collect::<Vec<_>>();
    H::Type_::from_vec(loc, ss)
}

fn single_types(context: &Context, ss: Vec<N::Type>) -> Vec<H::SingleType> {
    ss.into_iter().map(|s| single_type(context, s)).collect()
}

fn single_type(context: &Context, sp!(loc, ty_): N::Type) -> H::SingleType {
    use H::SingleType_ as HS;
    use N::Type_ as NT;
    let s_ = match ty_ {
        NT::Ref(mut_, nb) => HS::Ref(mut_, base_type(context, *nb)),
        _ => HS::Base(base_type(context, sp(loc, ty_))),
    };
    sp(loc, s_)
}

fn type_(context: &Context, sp!(loc, ty_): N::Type) -> H::Type {
    use H::Type_ as HT;
    use N::{TypeName_ as TN, Type_ as NT};
    let t_ = match ty_ {
        NT::Unit => HT::Unit,
        NT::Apply(None, n, tys) => {
            crate::shared::ast_debug::print_verbose(&NT::Apply(None, n, tys));
            panic!("ICE kind not expanded: {:#?}", loc)
        }
        NT::Apply(Some(_), sp!(_, TN::Multiple(_)), ss) => HT::Multiple(single_types(context, ss)),
        _ => HT::Single(single_type(context, sp(loc, ty_))),
    };
    sp(loc, t_)
}

//**************************************************************************************************
// Expression Processing
//**************************************************************************************************

// -------------------------------------------------------------------------------------------------
// HHelpers
// -------------------------------------------------------------------------------------------------
// These are defined first because the macro must before its usage because Rust won't figure out
// phasing for you..

fn divergent(stmt_: &H::Statement_) -> bool {
    // print!("Checking divergence for ");
    // crate::shared::ast_debug::print_verbose(stmt_);
    use H::{Command_ as C, Statement_ as S};

    macro_rules! h_stmt_cmd {
        ($cmd:pat) => {
            sp!(_, S::Command(sp!(_, $cmd)))
        };
    }

    macro_rules! hcmd {
        ($cmd:pat) => {
            S::Command(sp!(_, $cmd))
        };
    }

    fn divergent_while_block(block: &Block) -> bool {
        matches!(
            block.back(),
            Some(h_stmt_cmd!(C::Abort(_))) | Some(h_stmt_cmd!(C::Return { .. }))
        )
    }

    fn divergent_block(block: &Block) -> bool {
        matches!(
            block.back(),
            Some(h_stmt_cmd!(C::Break(_)))
                | Some(h_stmt_cmd!(C::Continue(_)))
                | Some(h_stmt_cmd!(C::Abort(_)))
                | Some(h_stmt_cmd!(C::Return { .. }))
        )
    }

    match stmt_ {
        S::IfElse {
            if_block,
            else_block,
            ..
        } => divergent_block(if_block) && divergent_block(else_block),

        // this is wholly unsatisfactory, and really we should nuke while during expansion.
        S::While { block, .. } => divergent_while_block(block),

        S::Loop { has_break, .. } => !has_break,

        hcmd!(C::Break(_))
        | hcmd!(C::Continue(_))
        | hcmd!(C::Abort(_))
        | hcmd!(C::Return { .. }) => true,

        _ => false,
    }
}

macro_rules! make_block {
    () => { VecDeque::new() };
    ($($elems:expr),+) => { VecDeque::from([$($elems),*]) };
}

fn make_command(loc: Loc, command: H::Command_) -> H::Statement {
    sp(loc, H::Statement_::Command(sp(loc, command)))
}

fn process_loop_body(context: &mut Context, body: T::Exp) -> H::Block {
    let mut loop_block = make_block!();
    statement(context, &mut loop_block, body);
    loop_block
}

fn tbool(loc: Loc) -> H::Type {
    H::Type_::bool(loc)
}

fn bool_exp(loc: Loc, value: bool) -> H::Exp {
    H::exp(
        tbool(loc),
        sp(
            loc,
            H::UnannotatedExp_::Value(sp(loc, H::Value_::Bool(value))),
        ),
    )
}

fn tunit(loc: Loc) -> H::Type {
    sp(loc, H::Type_::Unit)
}

fn unit_exp(loc: Loc) -> H::Exp {
    H::exp(
        tunit(loc),
        sp(
            loc,
            H::UnannotatedExp_::Unit {
                case: H::UnitCase::Implicit,
            },
        ),
    )
}

fn trailing_unit_exp(loc: Loc) -> H::Exp {
    H::exp(
        tunit(loc),
        sp(
            loc,
            H::UnannotatedExp_::Unit {
                case: H::UnitCase::Trailing,
            },
        ),
    )
}

fn maybe_freeze(
    context: &mut Context,
    block: &mut Block,
    expected_type_opt: Option<H::Type>,
    e: Option<H::Exp>,
) -> Option<H::Exp> {
    if let Some(exp) = e {
        if exp.is_unreachable() {
            Some(exp)
        } else if let Some(expected_type) = expected_type_opt {
            let (mut stmts, frozen_exp) = freeze(context, &expected_type, exp);
            block.append(&mut stmts);
            Some(frozen_exp)
        } else {
            Some(exp)
        }
    } else {
        e
    }
}

const DEAD_ERR_EXP: &str = "Invalid use of a divergent expression. The code following the \
                            evaluation of this expression will be dead and should be removed.";

fn emit_unreachable(context: &mut Context, loc: Loc) {
    context
        .env
        .add_diag(diag!(UnusedItem::DeadCode, (loc, DEAD_ERR_EXP)));
}

fn is_statement(e: &T::Exp) -> bool {
    use T::UnannotatedExp_ as E;
    matches!(
        e.exp.value,
        E::Return(_)
            | E::Abort(_)
            | E::Give(_, _)
            | E::Continue(_)
            | E::Assign(_, _, _)
            | E::Mutate(_, _)
    )
}

fn is_unit_statement(e: &T::Exp) -> bool {
    use T::UnannotatedExp_ as E;
    matches!(e.exp.value, E::Assign(_, _, _) | E::Mutate(_, _))
}

fn is_binop(e: &T::Exp) -> bool {
    use T::UnannotatedExp_ as E;
    matches!(e.exp.value, E::BinopExp(_, _, _, _))
}

// fn bind_for_short_circuit(e: &T::Exp) -> bool {
//     use T::UnannotatedExp_ as TE;
//     match &e.exp.value {
//         TE::Use(_) => panic!("ICE should have been expanded"),
//         TE::Value(_)
//         | TE::Constant(_, _)
//         | TE::Move { .. }
//         | TE::Copy { .. }
//         | TE::UnresolvedError => false,
//
//         // TODO might want to case ModuleCall for fake natives
//         TE::ModuleCall(_) => true,
//
//         TE::Block(seq) => bind_for_short_circuit_sequence(seq),
//         TE::Annotate(el, _) => bind_for_short_circuit(el),
//
//         TE::Break
//         | TE::Continue
//         | TE::IfElse(_, _, _)
//         | TE::While(_, _)
//         | TE::Loop { .. }
//         | TE::Return(_)
//         | TE::Abort(_)
//         | TE::Builtin(_, _)
//         | TE::Dereference(_)
//         | TE::UnaryExp(_, _)
//         | TE::Borrow(_, _, _)
//         | TE::TempBorrow(_, _)
//         | TE::BinopExp(_, _, _, _) => true,
//
//         TE::Unit { .. }
//         | TE::Spec(_, _)
//         | TE::Assign(_, _, _)
//         | TE::Mutate(_, _)
//         | TE::Pack(_, _, _, _)
//         | TE::Vector(_, _, _, _)
//         | TE::BorrowLocal(_, _)
//         | TE::ExpList(_)
//         | TE::Cast(_, _) => panic!("ICE unexpected exp in short circuit check: {:?}", e),
//     }
// }
//
// fn bind_for_short_circuit_sequence(seq: &T::Sequence) -> bool {
//     use T::SequenceItem_ as TItem;
//     seq.len() != 1
//         || match &seq[0].value {
//             TItem::Seq(e) => bind_for_short_circuit(e),
//             item @ TItem::Declare(_) | item @ TItem::Bind(_, _, _) => {
//                 panic!("ICE unexpected item in short circuit check: {:?}", item)
//             }
//         }
// }

fn emit_trailing_semicolon_error(context: &mut Context, terminal_loc: Loc, semi_loc: Loc) {
    let semi_msg = "Invalid trailing ';'";
    let unreachable_msg = "Any code after this expression will not be reached";
    let info_msg = "A trailing ';' in an expression block implicitly adds a '()' value \
                after the semicolon. That '()' value will not be reachable";
    context.env.add_diag(diag!(
        UnusedItem::TrailingSemi,
        (semi_loc, semi_msg),
        (terminal_loc, unreachable_msg),
        (semi_loc, info_msg),
    ));
}

fn trailing_unit(seq: &T::Sequence) -> bool {
    use T::SequenceItem_ as S;
    if let Some(sp!(_, S::Seq(exp))) = &seq.back() {
        matches!(exp.exp.value, T::UnannotatedExp_::Unit { trailing: true })
    } else {
        false
    }
}

// -------------------------------------------------------------------------------------------------
// Tail Position
// -------------------------------------------------------------------------------------------------

fn body(
    context: &mut Context,
    expected_type: Option<&H::Type>,
    loc: Loc,
    seq: T::Sequence,
) -> (Block, Option<H::Exp>) {
    if seq.is_empty() {
        (make_block!(), Some(unit_exp(loc)))
    } else {
        let mut block = make_block!();
        let final_exp = tail_block(context, &mut block, expected_type, seq);
        (block, final_exp)
    }
}

fn tail(
    context: &mut Context,
    block: &mut Block,
    expected_type: Option<&H::Type>,
    e: T::Exp,
) -> Option<H::Exp> {
    // print!("stmt");
    // crate::shared::ast_debug::print_verbose(&e);
    if is_statement(&e) {
        let result = if is_unit_statement(&e) {
            Some(unit_exp(e.exp.loc))
        } else {
            None
        };
        statement(context, block, e);
        return result;
    }

    use H::Statement_ as S;
    use T::UnannotatedExp_ as E;
    let T::Exp {
        ty: ref in_type,
        exp: sp!(eloc, e_),
    } = e;
    let out_type = type_(context, in_type.clone());

    match e_ {
        // -----------------------------------------------------------------------------------------
        // control flow statements
        // -----------------------------------------------------------------------------------------
        E::IfElse(test, conseq, alt) => {
            let cond = value(context, block, Some(&tbool(eloc)), *test);
            let mut if_block = make_block!();
            let conseq_exp = tail(context, &mut if_block, Some(&out_type), *conseq);
            let mut else_block = make_block!();
            let alt_exp = tail(context, &mut else_block, Some(&out_type), *alt);

            let (binders, bound_exp) = make_binders(context, eloc, out_type.clone());

            let if_binds = bind_value_in_block(
                context,
                binders.clone(),
                Some(out_type.clone()),
                &mut if_block,
                conseq_exp,
            );
            let else_binds =
                bind_value_in_block(context, binders, Some(out_type), &mut else_block, alt_exp);

            if let Some(cond) = cond {
                let if_else = S::IfElse {
                    cond: Box::new(cond),
                    if_block,
                    else_block,
                };
                block.push_back(sp(eloc, if_else));
                if if_binds || else_binds {
                    Some(bound_exp)
                } else {
                    None
                }
            } else {
                None
            }
        }
        // While loops can't yield values, so we treat them as statements with no binders.
        e_ @ E::While(_, _, _) => {
            statement(context, block, T::exp(in_type.clone(), sp(eloc, e_)));
            Some(trailing_unit_exp(eloc))
        }
        E::Loop {
            name,
            has_break: true,
            body,
        } => {
            let name = translate_var(name);
            let (binders, bound_exp) = make_binders(context, eloc, out_type.clone());
            let result = Some(if binders.is_empty() {
                // need to swap the implicit unit out for a trailing unit in tail position
                trailing_unit_exp(eloc)
            } else {
                bound_exp
            });
            context.record_named_block_binders(name, binders);
            context.record_named_block_type(name, out_type.clone());
            block.push_back(sp(
                eloc,
                S::Loop {
                    name,
                    has_break: true,
                    block: process_loop_body(context, *body),
                },
            ));
            result
        }
        e_ @ E::Loop { .. } => {
            // A loop wthout a break has no concrete type for its binders, but since we'll never
            // find a break we won't need binders anyway. We just treat it like a statement.
            statement(context, block, T::exp(in_type.clone(), sp(eloc, e_)));
            None
        }
        E::Block(seq) => tail_block(context, block, Some(&out_type), seq),

        // -----------------------------------------------------------------------------------------
        //  statements that need to be hoisted out
        // -----------------------------------------------------------------------------------------
        E::Return(_)
        | E::Abort(_)
        | E::Give(_, _)
        | E::Continue(_)
        | E::Assign(_, _, _)
        | E::Mutate(_, _) => panic!("ICE statement mishandled"),

        // -----------------------------------------------------------------------------------------
        //  value-like expression
        // -----------------------------------------------------------------------------------------
        e_ => {
            let e = T::Exp {
                ty: in_type.clone(),
                exp: sp(eloc, e_),
            };
            value(context, block, expected_type, e)
        }
    }
}

fn tail_block(
    context: &mut Context,
    block: &mut Block,
    expected_type: Option<&H::Type>,
    mut seq: T::Sequence,
) -> Option<H::Exp> {
    use T::SequenceItem_ as S;
    let has_trailing_unit = trailing_unit(&seq);
    let last_exp = seq.pop_back();
    statement_block(context, block, seq, false);
    // println!("Terminal: {:?}", terminal);
    // println!("Last Exp: {:?}", last_exp);
    match last_exp {
        None => None,
        Some(sp!(_, S::Seq(last))) if has_trailing_unit => match block.iter().last() {
            Some(sp!(sloc, stmt)) if divergent(stmt) => {
                emit_trailing_semicolon_error(context, *sloc, last.exp.loc);
                None
            }
            _ => tail(context, block, expected_type, *last),
        },
        Some(sp!(_, S::Seq(last))) => tail(context, block, expected_type, *last),
        Some(_) => panic!("ICE last sequence item should be an exp"),
    }
}

// -------------------------------------------------------------------------------------------------
// Value Position
// -------------------------------------------------------------------------------------------------

fn value(
    context: &mut Context,
    block: &mut Block,
    expected_type: Option<&H::Type>,
    e: T::Exp,
) -> Option<H::Exp> {
    // we pull outthese cases because it's easier to process them without destructuring `e` first.
    if is_statement(&e) {
        let result = if is_unit_statement(&e) {
            Some(unit_exp(e.exp.loc))
        } else {
            emit_unreachable(context, e.exp.loc);
            None
        };
        statement(context, block, e);
        return result;
    } else if is_binop(&e) {
        let out_type = type_(context, e.ty.clone());
        return process_binops(context, block, out_type, e);
    }

    use H::{Command_ as C, Statement_ as S, UnannotatedExp_ as HE};
    use T::UnannotatedExp_ as E;
    let T::Exp {
        ty: ref in_type,
        exp: sp!(eloc, e_),
    } = e;
    let out_type = type_(context, in_type.clone());
    let make_exp = |exp| Some(H::exp(out_type.clone(), sp(eloc, exp)));

    let preresult: Option<H::Exp> = match e_ {
        // ---------------------------------------------------------------------------------------
        // Expansion-y things
        // These could likely be discharged during expansion instead.
        //
        E::Builtin(bt, arguments) if matches!(&*bt, sp!(_, T::BuiltinFunction_::Assert(false))) => {
            use T::ExpListItem as TI;
            let [cond_item, code_item]: [TI; 2] = match arguments.exp.value {
                E::ExpList(arg_list) => arg_list.try_into().unwrap(),
                _ => panic!("ICE type checking failed"),
            };
            let (econd, ecode) = match (cond_item, code_item) {
                (TI::Single(econd, _), TI::Single(ecode, _)) => (econd, ecode),
                _ => panic!("ICE type checking failed"),
            };
            let cond_value = value(context, block, Some(&tbool(eloc)), econd);
            let code_value = value(context, block, None, ecode);
            if let (Some(cond), Some(code)) = (cond_value, code_value) {
                let if_block = make_block!();
                let else_block = make_block!(make_command(eloc, C::Abort(code)));
                block.push_back(sp(
                    eloc,
                    S::IfElse {
                        cond: Box::new(cond),
                        if_block,
                        else_block,
                    },
                ));
            }
            Some(unit_exp(eloc))
        }
        E::Builtin(bt, arguments) if matches!(&*bt, sp!(_, T::BuiltinFunction_::Assert(true))) => {
            use T::ExpListItem as TI;
            let [cond_item, code_item]: [TI; 2] = match arguments.exp.value {
                E::ExpList(arg_list) => arg_list.try_into().unwrap(),
                _ => panic!("ICE type checking failed"),
            };
            let (econd, ecode) = match (cond_item, code_item) {
                (TI::Single(econd, _), TI::Single(ecode, _)) => (econd, ecode),
                _ => panic!("ICE type checking failed"),
            };
            let cond_value = value(context, block, Some(&tbool(eloc)), econd);
            let mut else_block = make_block!();
            let code_value = value(context, &mut else_block, None, ecode);
            if let (Some(cond), Some(code)) = (cond_value, code_value) {
                let if_block = make_block!();
                else_block.push_back(make_command(eloc, C::Abort(code)));
                block.push_back(sp(
                    eloc,
                    S::IfElse {
                        cond: Box::new(cond),
                        if_block,
                        else_block,
                    },
                ));
            }
            Some(unit_exp(eloc))
        }

        // -----------------------------------------------------------------------------------------
        // control flow statements
        // -----------------------------------------------------------------------------------------
        E::IfElse(test, conseq, alt) => {
            let cond = value(context, block, Some(&tbool(eloc)), *test);
            let mut if_block = make_block!();
            let conseq_exp = tail(context, &mut if_block, Some(&out_type), *conseq);
            let mut else_block = make_block!();
            let alt_exp = tail(context, &mut else_block, Some(&out_type), *alt);

            let (binders, bound_exp) = make_binders(context, eloc, out_type.clone());

            let if_binds = bind_value_in_block(
                context,
                binders.clone(),
                Some(out_type.clone()),
                &mut if_block,
                conseq_exp,
            );
            let else_binds =
                bind_value_in_block(context, binders, Some(out_type), &mut else_block, alt_exp);

            if let Some(cond) = cond {
                let if_else = S::IfElse {
                    cond: Box::new(cond),
                    if_block,
                    else_block,
                };
                block.push_back(sp(eloc, if_else));
                if if_binds || else_binds {
                    Some(bound_exp)
                } else {
                    None
                }
            } else {
                None
            }
        }
        // While loops can't yield values, so we treat them as statements with no binders.
        e_ @ E::While(_, _, _) => {
            statement(context, block, T::exp(in_type.clone(), sp(eloc, e_)));
            Some(unit_exp(eloc))
        }
        E::Loop {
            name,
            has_break: true,
            body,
        } => {
            let name = translate_var(name);
            let (binders, bound_exp) = make_binders(context, eloc, out_type.clone());
            context.record_named_block_binders(name, binders);
            context.record_named_block_type(name, out_type.clone());
            block.push_back(sp(
                eloc,
                S::Loop {
                    name,
                    has_break: true,
                    block: process_loop_body(context, *body),
                },
            ));
            Some(bound_exp)
        }
        e_ @ E::Loop { .. } => {
            emit_unreachable(context, eloc);
            statement(context, block, T::exp(in_type.clone(), sp(eloc, e_)));
            None
        }
        E::Block(seq) => value_block(context, block, Some(&out_type), seq),

        // -----------------------------------------------------------------------------------------
        //  calls
        // -----------------------------------------------------------------------------------------
        E::ModuleCall(call) => {
            let T::ModuleCall {
                module,
                name,
                type_arguments,
                arguments,
                parameter_types,
                acquires,
            } = *call;
            let htys = base_types(context, type_arguments);
            let expected_type = H::Type_::from_vec(eloc, single_types(context, parameter_types));
            let maybe_arguments = value_list(context, block, Some(&expected_type), *arguments);
            if let Some(arguments) = maybe_arguments {
                let call = H::ModuleCall {
                    module,
                    name,
                    type_arguments: htys,
                    arguments,
                    acquires,
                };
                make_exp(HE::ModuleCall(Box::new(call)))
            } else {
                None
            }
        }
        E::Builtin(bt, args) => builtin(context, block, eloc, *bt, args).and_then(make_exp),

        // -----------------------------------------------------------------------------------------
        // nested expressions
        // -----------------------------------------------------------------------------------------
        E::Vector(vec_loc, size, vty, args) => {
            let maybe_values = value_list(context, block, None, *args);
            if let Some(values) = maybe_values {
                make_exp(HE::Vector(
                    vec_loc,
                    size,
                    Box::new(base_type(context, *vty)),
                    values,
                ))
            } else {
                None
            }
        }
        E::Dereference(ev) => {
            let value = value(context, block, None, *ev);
            if let Some(value) = value {
                make_exp(HE::Dereference(Box::new(value)))
            } else {
                None
            }
        }
        E::UnaryExp(op, operand) => {
            let op_value = value(context, block, None, *operand);
            if let Some(operand) = op_value {
                make_exp(HE::UnaryExp(op, Box::new(operand)))
            } else {
                None
            }
        }

        E::Pack(module_ident, struct_name, arg_types, fields) => {
            // all fields of a packed struct type are used
            context
                .used_fields
                .entry(struct_name.value())
                .or_insert_with(BTreeSet::new)
                .extend(fields.iter().map(|(_, name, _)| *name));

            let base_types = base_types(context, arg_types);

            let decl_fields = context.fields(&module_ident, &struct_name);

            let mut texp_fields: Vec<(usize, Field, usize, N::Type, T::Exp)> =
                if let Some(field_map) = decl_fields {
                    fields
                        .into_iter()
                        .map(|(f, (exp_idx, (bt, tf)))| {
                            (*field_map.get(&f).unwrap(), f, exp_idx, bt, tf)
                        })
                        .collect()
                } else {
                    // If no field map, compiler error in typing.
                    fields
                        .into_iter()
                        .enumerate()
                        .map(|(ndx, (f, (exp_idx, (bt, tf))))| (ndx, f, exp_idx, bt, tf))
                        .collect()
                };
            texp_fields.sort_by(|(_, _, eidx1, _, _), (_, _, eidx2, _, _)| eidx1.cmp(eidx2));

            let reorder_fields = texp_fields
                .iter()
                .any(|(decl_idx, _, exp_idx, _, _)| decl_idx != exp_idx);

            let fields = if !reorder_fields {
                let mut fields = vec![];
                let field_exps = texp_fields
                    .into_iter()
                    .map(|(_, f, _, bt, te)| {
                        let bt = base_type(context, bt);
                        fields.push((f, bt.clone()));
                        let t = H::Type_::base(bt);
                        (te, Some(t))
                    })
                    .collect();
                let field_exps = value_evaluation_order(context, block, field_exps);
                assert!(
                    fields.len() == field_exps.len(),
                    "ICE exp_evaluation_order changed arity"
                );
                field_exps
                    .into_iter()
                    .zip(fields)
                    .map(|(e, (f, bt))| (f, bt, e))
                    .collect()
            } else {
                let num_fields = decl_fields.as_ref().map(|m| m.len()).unwrap_or(0);
                let mut fields = (0..num_fields).map(|_| None).collect::<Vec<_>>();
                for (decl_idx, field, _exp_idx, bt, tf) in texp_fields {
                    // Might have too many arguments, there will be an error from typing
                    if decl_idx >= fields.len() {
                        debug_assert!(context.env.has_errors());
                        break;
                    }
                    let base_ty = base_type(context, bt);
                    let t = H::Type_::base(base_ty.clone());
                    let field_expr = value(context, block, Some(&t), tf);
                    assert!(fields.get(decl_idx).unwrap().is_none());
                    assert!(field_expr.is_some());
                    let move_tmp = bind_exp(context, block, field_expr.unwrap());
                    fields[decl_idx] = Some((field, base_ty, move_tmp))
                }
                // Might have too few arguments, there will be an error from typing if so
                fields
                    .into_iter()
                    .filter_map(|o| {
                        // if o is None, context should have errors
                        debug_assert!(o.is_some() || context.env.has_errors());
                        o
                    })
                    .collect()
            };
            make_exp(HE::Pack(struct_name, base_types, fields))
        }

        E::ExpList(items) => {
            let mut values = Vec::new();
            for item in items {
                match item {
                    T::ExpListItem::Single(entry, ty) => {
                        let exp_ty = single_type(context, *ty);
                        let new_value =
                            value(context, block, Some(&H::Type_::single(exp_ty)), entry);
                        values.push(new_value.unwrap());
                    }
                    T::ExpListItem::Splat(_, _, _) => {
                        panic!("ICE splats should be lowered already")
                    }
                }
            }
            make_exp(HE::Multiple(values))
        }

        E::Borrow(mut_, base_exp, field) => {
            let exp = value(context, block, None, *base_exp);
            if let Some(exp) = exp {
                if let Some(struct_name) = struct_name(&exp.ty) {
                    context
                        .used_fields
                        .entry(struct_name.value())
                        .or_insert_with(BTreeSet::new)
                        .insert(field.value());
                }
                make_exp(HE::Borrow(mut_, Box::new(exp), field))
            } else {
                exp
            }
        }
        E::TempBorrow(mut_, base_exp) => {
            let exp = value(context, block, None, *base_exp);
            match exp {
                Some(exp) => {
                    let bound_exp = bind_exp(context, block, exp);
                    let tmp = match bound_exp.exp.value {
                        HE::Move {
                            annotation: MoveOpAnnotation::InferredLastUsage,
                            var,
                        } => var,
                        _ => panic!("ICE invalid bind_exp for single value"),
                    };
                    make_exp(HE::BorrowLocal(mut_, tmp))
                }
                None => None,
            }
        }
        E::BorrowLocal(mut_, var) => make_exp(HE::BorrowLocal(mut_, translate_var(var))),
        E::Cast(base, rhs_ty) => {
            use N::BuiltinTypeName_ as BT;
            let new_base = value(context, block, None, *base);
            let bt = match rhs_ty.value.builtin_name() {
                Some(bt @ sp!(_, BT::U8))
                | Some(bt @ sp!(_, BT::U16))
                | Some(bt @ sp!(_, BT::U32))
                | Some(bt @ sp!(_, BT::U64))
                | Some(bt @ sp!(_, BT::U128))
                | Some(bt @ sp!(_, BT::U256)) => bt.clone(),
                _ => panic!("ICE typing failed for cast"),
            };
            if let Some(base) = new_base {
                make_exp(HE::Cast(Box::new(base), bt))
            } else {
                None
            }
        }
        E::Annotate(base, rhs_ty) => {
            let annotated_type = type_(context, *rhs_ty);
            value(context, block, Some(&annotated_type), *base)
        }

        // -----------------------------------------------------------------------------------------
        // value-based expressions without subexpressions -- translate these directly
        // -----------------------------------------------------------------------------------------
        E::Unit { trailing } => {
            let new_unit = HE::Unit {
                case: if trailing {
                    H::UnitCase::Trailing
                } else {
                    H::UnitCase::FromUser
                },
            };
            make_exp(new_unit)
        }
        E::Value(ev) => make_exp(HE::Value(process_value(ev))),
        E::Constant(_m, c) => make_exp(HE::Constant(c)), // only private constants (for now)
        E::Move { from_user, var } => {
            let annotation = if from_user {
                MoveOpAnnotation::FromUser
            } else {
                MoveOpAnnotation::InferredNoCopy
            };
            let var = translate_var(var);
            make_exp(HE::Move { annotation, var })
        }
        E::Copy { from_user, var } => {
            let var = translate_var(var);
            make_exp(HE::Copy { from_user, var })
        }

        // -----------------------------------------------------------------------------------------
        //  matches that handled earlier
        // -----------------------------------------------------------------------------------------
        E::BinopExp(_, _, _, _)
        | E::Return(_)
        | E::Abort(_)
        | E::Give(_, _)
        | E::Continue(_)
        | E::Assign(_, _, _)
        | E::Mutate(_, _) => panic!("ICE statement mishandled"),

        // -----------------------------------------------------------------------------------------
        // odds and ends -- things we need to deal with but that don't do much
        // -----------------------------------------------------------------------------------------
        E::Use(_) => panic!("ICE unexpanded use"),

        E::Spec(u, tused_locals) => {
            let used_locals = tused_locals
                .into_iter()
                .map(|(var, ty)| {
                    let v = translate_var(var);
                    let st = single_type(context, ty);
                    (v, st)
                })
                .collect();
            make_exp(HE::Spec(u, used_locals))
        }

        E::UnresolvedError => {
            assert!(context.env.has_errors());
            make_exp(HE::UnresolvedError)
        }
    };
    maybe_freeze(context, block, expected_type.cloned(), preresult)
}

fn value_block(
    context: &mut Context,
    block: &mut Block,
    expected_type: Option<&H::Type>,
    mut seq: T::Sequence,
) -> Option<H::Exp> {
    use T::SequenceItem_ as S;
    let last_exp = seq.pop_back();
    statement_block(context, block, seq, false);
    match last_exp {
        None => None,
        Some(sp!(_, S::Seq(last))) => value(context, block, expected_type, *last),
        Some(_) => panic!("ICE last sequence item should be an exp"),
    }
}

fn value_list(
    context: &mut Context,
    block: &mut Block,
    ty: Option<&H::Type>,
    e: T::Exp,
) -> Option<Vec<H::Exp>> {
    use H::Type_ as HT;
    use T::UnannotatedExp_ as TE;
    if let TE::ExpList(items) = e.exp.value {
        assert!(!items.is_empty());
        let mut tys = vec![];
        let mut item_exprs = vec![];
        let expected_tys: Vec<_> = if let Some(sp!(tloc, HT::Multiple(ts))) = ty {
            ts.iter()
                .map(|t| Some(sp(*tloc, HT::Single(t.clone()))))
                .collect()
        } else {
            items.iter().map(|_| None).collect()
        };
        for (item, expected_ty) in items.into_iter().zip(expected_tys) {
            match item {
                T::ExpListItem::Single(te, ts) => {
                    let t = single_type(context, *ts);
                    tys.push(t);
                    item_exprs.push((te, expected_ty));
                }
                T::ExpListItem::Splat(_, _, _) => panic!("ICE spalt is unsupported."),
            }
        }
        let exprs = value_evaluation_order(context, block, item_exprs);
        assert!(
            exprs.len() == tys.len(),
            "ICE value_evaluation_order changed arity"
        );
        Some(exprs)
    } else if let TE::Unit { .. } = e.exp.value {
        Some(vec![])
    } else {
        let exp = value(context, block, ty, e);
        // FIXME(cgswords): check expr is defined; error otherwise.
        exp.map(|e| vec![e])
    }
}

// -------------------------------------------------------------------------------------------------
// Statement Position
// -------------------------------------------------------------------------------------------------

fn statement(context: &mut Context, block: &mut Block, e: T::Exp) {
    // print!("stmt");
    // crate::shared::ast_debug::print_verbose(&e);
    use H::{Command_ as C, Statement_ as S};
    use T::UnannotatedExp_ as E;

    let T::Exp {
        ty,
        exp: sp!(eloc, e_),
    } = e;

    let make_exp = |e_| T::Exp {
        ty: ty.clone(),
        exp: sp(eloc, e_),
    };
    match e_ {
        // -----------------------------------------------------------------------------------------
        // control flow statements
        // -----------------------------------------------------------------------------------------
        E::IfElse(test, conseq, alt) => {
            let cond = value(context, block, Some(&tbool(eloc)), *test);
            let mut if_block = make_block!();
            statement(context, &mut if_block, *conseq);
            let mut else_block = make_block!();
            statement(context, &mut else_block, *alt);
            if let Some(cond) = cond {
                block.push_back(sp(
                    eloc,
                    S::IfElse {
                        cond: Box::new(cond),
                        if_block,
                        else_block,
                    },
                ));
            }
        }
        E::While(name, test, body) => {
            let name = translate_var(name);
            // While loops can still use break and continue so we build them dummy binders.
            context.record_named_block_binders(name, vec![]);
            context.record_named_block_type(name, tunit(eloc));
            let mut cond_block = make_block!();
            let cond_exp = value(context, &mut cond_block, Some(&tbool(eloc)), *test);
            let mut body_block = make_block!();
            statement(context, &mut body_block, *body);
            if let Some(cond_exp) = cond_exp {
                let cond = (cond_block, Box::new(cond_exp));
                block.push_back(sp(
                    eloc,
                    S::While {
                        name,
                        cond,
                        block: body_block,
                    },
                ));
            } else {
                block.append(&mut cond_block);
            }
        }
        E::Loop {
            name,
            body,
            has_break,
        } => {
            let name = translate_var(name);
            let out_type = type_(context, ty.clone());
            let (binders, bound_exp) = make_binders(context, eloc, out_type.clone());
            let unused_binders = !binders.is_empty() && has_break;
            context.record_named_block_binders(name, binders);
            context.record_named_block_type(name, out_type);
            block.push_back(sp(
                eloc,
                S::Loop {
                    name,
                    has_break,
                    block: process_loop_body(context, *body),
                },
            ));
            if unused_binders {
                let msg = "This loop's breaks value(s) are unused";
                context
                    .env
                    .add_diag(diag!(UnusedItem::LoopBreakValue, (eloc, msg)));
                make_ignore_and_pop(block, Some(bound_exp));
            }
        }
        E::Block(seq) => statement_block(context, block, seq, true),
        E::Return(rhs) => {
            let expected_type = context.signature.as_ref().map(|s| s.return_type.clone());
            let rhs = value(context, block, expected_type.as_ref(), *rhs);
            if let Some(exp) = rhs {
                let ret_command = C::Return {
                    from_user: true,
                    exp,
                };
                block.push_back(make_command(eloc, ret_command));
            }
        }
        E::Abort(rhs) => {
            let rhs = value(context, block, None, *rhs);
            if let Some(rhs_exp) = rhs {
                block.push_back(make_command(eloc, C::Abort(rhs_exp)));
            }
        }
        E::Give(name, rhs) => {
            let out_name = translate_var(name);
            let bind_ty = context.lookup_named_block_type(&out_name);
            let rhs = value(context, block, bind_ty.as_ref(), *rhs);
            let binders = context.lookup_named_block_binders(&out_name);
            if binders.is_empty() {
                make_ignore_and_pop(block, rhs);
            } else {
                bind_value_in_block(context, binders, bind_ty, block, rhs);
            }
            block.push_back(make_command(eloc, C::Break(out_name)));
        }
        E::Continue(name) => {
            let out_name = translate_var(name);
            block.push_back(make_command(eloc, C::Continue(out_name)));
        }

        // -----------------------------------------------------------------------------------------
        //  statements with effects
        // -----------------------------------------------------------------------------------------
        E::Assign(assigns, lvalue_ty, rhs) => {
            let expected_type = expected_types(context, eloc, lvalue_ty);
            let rhs = value(context, block, Some(&expected_type), *rhs);
            if let Some(exp) = rhs {
                make_assignments(context, block, eloc, assigns, exp);
            }
        }

        E::Mutate(lhs_in, rhs_in) => {
            // evaluate RHS first
            let rhs = value(context, block, None, *rhs_in);
            let lhs = value(context, block, None, *lhs_in);
            if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
                block.push_back(make_command(eloc, C::Mutate(Box::new(lhs), Box::new(rhs))));
            }
        }

        // calls might be for effect
        e_ @ E::ModuleCall(_) => value_statement(context, block, make_exp(e_)),
        e_ @ E::Builtin(_, _) => value_statement(context, block, make_exp(e_)),

        // -----------------------------------------------------------------------------------------
        // valued expressions -- when these occur in statement position need their children
        // unravelled to find any embedded, effectful operations. We unravel those and discard the
        // results. These cases could be synthesized as ignore_and_pop but we avoid them altogether
        // -----------------------------------------------------------------------------------------

        // FIXME(cgswords): we can't optimize because almost all of these throw. We have to do the
        // "honest" work here, even though it's thrown away. Consider emitting a warning about
        // these and/or eliminating them in Move 2024.
        e_ @ E::Vector(_, _, _, _)
        | e_ @ E::Dereference(_)
        | e_ @ E::UnaryExp(_, _)
        | e_ @ E::BinopExp(_, _, _, _)
        | e_ @ E::Pack(_, _, _, _)
        | e_ @ E::ExpList(_)
        | e_ @ E::Borrow(_, _, _)
        | e_ @ E::TempBorrow(_, _)
        | e_ @ E::Cast(_, _)
        | e_ @ E::Annotate(_, _)
        | e_ @ E::BorrowLocal(_, _)
        | e_ @ E::Constant(_, _)
        | e_ @ E::Move { .. }
        | e_ @ E::Copy { .. }
        | e_ @ E::Spec(..)
        | e_ @ E::UnresolvedError => value_statement(context, block, make_exp(e_)),

        E::Value(_) | E::Unit { .. } => (),

        // -----------------------------------------------------------------------------------------
        // odds and ends -- things we need to deal with but that don't do much
        // -----------------------------------------------------------------------------------------
        E::Use(_) => panic!("ICE unexpanded use"),
    }
}

fn statement_block(context: &mut Context, block: &mut Block, seq: T::Sequence, stmt_pos: bool) {
    // println!("=> stmt block");
    use T::SequenceItem_ as S;
    // println!("statement block");
    let has_trailing_unit = stmt_pos && trailing_unit(&seq);
    let last_ndx = seq.iter().skip(1).len();
    for (ndx, sp!(sloc, seq_item)) in seq.into_iter().enumerate() {
        // println!("terminal: {:?}", terminal);
        // println!("item: {:?}", seq_item);
        match seq_item {
            S::Seq(last) if ndx == last_ndx && has_trailing_unit => match block.iter().last() {
                Some(sp!(sloc, stmt)) if divergent(stmt) => {
                    emit_trailing_semicolon_error(context, *sloc, last.exp.loc);
                }
                _ => statement(context, block, *last),
            },
            S::Seq(te) => statement(context, block, *te),
            S::Declare(bindings) => {
                declare_bind_list(context, &bindings);
            }
            S::Bind(bindings, ty, expr) => {
                let expected_tys = expected_types(context, sloc, ty);
                let rhs_exp = value(context, block, Some(&expected_tys), *expr);
                if let Some(rhs_exp) = rhs_exp {
                    declare_bind_list(context, &bindings);
                    make_assignments(context, block, sloc, bindings, rhs_exp);
                }
            }
        }
    }
}

// Treat something like a value, and add a final `ignore_and_pop` at the end to consume that value.
fn value_statement(context: &mut Context, block: &mut Block, e: T::Exp) {
    let exp = value(context, block, None, e);
    make_ignore_and_pop(block, exp)
}

//**************************************************************************************************
// LValue
//**************************************************************************************************

fn declare_bind_list(context: &mut Context, sp!(_, binds): &T::LValueList) {
    binds.iter().for_each(|b| declare_bind(context, b))
}

fn declare_bind(context: &mut Context, sp!(_, bind_): &T::LValue) {
    use T::LValue_ as L;
    match bind_ {
        L::Ignore => (),
        L::Var { var: v, ty, .. } => {
            let st = single_type(context, *ty.clone());
            context.bind_local(*v, st)
        }
        L::Unpack(_, _, _, fields) | L::BorrowUnpack(_, _, _, _, fields) => fields
            .iter()
            .for_each(|(_, _, (_, (_, b)))| declare_bind(context, b)),
    }
}

fn make_assignments(
    context: &mut Context,
    result: &mut Block,
    loc: Loc,
    sp!(_, assigns): T::LValueList,
    rvalue: H::Exp,
) {
    use H::{Command_ as C, Statement_ as S};
    let mut lvalues = vec![];
    let mut after = Block::new();
    for (idx, a) in assigns.into_iter().enumerate() {
        let a_ty = rvalue.ty.value.type_at_index(idx);
        let (ls, mut af) = assign(context, a, a_ty);

        lvalues.push(ls);
        after.append(&mut af);
    }
    result.push_back(sp(loc, S::Command(sp(loc, C::Assign(lvalues, rvalue)))));
    result.append(&mut after);
}

fn assign(
    context: &mut Context,
    sp!(loc, ta_): T::LValue,
    rvalue_ty: &H::SingleType,
) -> (H::LValue, Block) {
    use H::{LValue_ as L, UnannotatedExp_ as E};
    use T::LValue_ as A;
    let mut after = Block::new();
    let l_ = match ta_ {
        A::Ignore => L::Ignore,
        A::Var { var: v, ty: st, .. } => {
            L::Var(translate_var(v), Box::new(single_type(context, *st)))
        }
        A::Unpack(m, s, tbs, tfields) => {
            // all fields of an unpacked struct type are used
            context
                .used_fields
                .entry(s.value())
                .or_insert_with(BTreeSet::new)
                .extend(tfields.iter().map(|(_, s, _)| *s));

            let bs = base_types(context, tbs);

            let mut fields = vec![];
            for (decl_idx, f, bt, tfa) in assign_fields(context, &m, &s, tfields) {
                assert!(fields.len() == decl_idx);
                let st = &H::SingleType_::base(bt);
                let (fa, mut fafter) = assign(context, tfa, st);
                after.append(&mut fafter);
                fields.push((f, fa))
            }
            L::Unpack(s, bs, fields)
        }
        A::BorrowUnpack(mut_, m, s, _tss, tfields) => {
            // all fields of an unpacked struct type are used
            context
                .used_fields
                .entry(s.value())
                .or_insert_with(BTreeSet::new)
                .extend(tfields.iter().map(|(_, s, _)| *s));

            let tmp = context.new_temp(loc, rvalue_ty.clone());
            let copy_tmp = || {
                let copy_tmp_ = E::Copy {
                    from_user: false,
                    var: tmp,
                };
                H::exp(H::Type_::single(rvalue_ty.clone()), sp(loc, copy_tmp_))
            };
            let fields = assign_fields(context, &m, &s, tfields)
                .into_iter()
                .enumerate();
            for (idx, (decl_idx, f, bt, tfa)) in fields {
                assert!(idx == decl_idx);
                let floc = tfa.loc;
                let borrow_ = E::Borrow(mut_, Box::new(copy_tmp()), f);
                let borrow_ty = H::Type_::single(sp(floc, H::SingleType_::Ref(mut_, bt)));
                let borrow = H::exp(borrow_ty, sp(floc, borrow_));
                make_assignments(context, &mut after, floc, sp(floc, vec![tfa]), borrow);
            }
            L::Var(tmp, Box::new(rvalue_ty.clone()))
        }
    };
    (sp(loc, l_), after)
}

fn assign_fields(
    context: &Context,
    m: &ModuleIdent,
    s: &StructName,
    tfields: Fields<(N::Type, T::LValue)>,
) -> Vec<(usize, Field, H::BaseType, T::LValue)> {
    let decl_fields = context.fields(m, s);
    let mut count = 0;
    let mut decl_field = |f: &Field| -> usize {
        match decl_fields {
            Some(m) => *m.get(f).unwrap(),
            None => {
                // none can occur with errors in typing
                let i = count;
                count += 1;
                i
            }
        }
    };
    let mut tfields_vec = tfields
        .into_iter()
        .map(|(f, (_idx, (tbt, tfa)))| (decl_field(&f), f, base_type(context, tbt), tfa))
        .collect::<Vec<_>>();
    tfields_vec.sort_by(|(idx1, _, _, _), (idx2, _, _, _)| idx1.cmp(idx2));
    tfields_vec
}

//**************************************************************************************************
// Commands
//**************************************************************************************************

fn make_ignore_and_pop(block: &mut Block, e: Option<H::Exp>) {
    use H::UnannotatedExp_ as E;
    if let Some(exp) = e {
        let loc = exp.exp.loc;
        match &exp.ty.value {
            H::Type_::Unit => match exp.exp.value {
                E::Unit { .. } => (),
                E::Value(_) => (),
                _ => {
                    let c = sp(loc, H::Command_::IgnoreAndPop { pop_num: 0, exp });
                    block.push_back(sp(loc, H::Statement_::Command(c)));
                }
            },
            H::Type_::Single(_) => {
                let c = sp(loc, H::Command_::IgnoreAndPop { pop_num: 1, exp });
                block.push_back(sp(loc, H::Statement_::Command(c)));
            }
            H::Type_::Multiple(tys) => {
                let c = sp(
                    loc,
                    H::Command_::IgnoreAndPop {
                        pop_num: tys.len(),
                        exp,
                    },
                );
                block.push_back(sp(loc, H::Statement_::Command(c)));
            }
        };
    }
}

//**************************************************************************************************
// Expressions
//**************************************************************************************************

fn struct_name(sp!(_, t): &H::Type) -> Option<StructName> {
    let H::Type_::Single(st) = t else {
        return None;
    };
    let bt = match &st.value {
        H::SingleType_::Base(bt) => bt,
        H::SingleType_::Ref(_, bt) => bt,
    };
    let H::BaseType_::Apply(_, tname ,_ ) = &bt.value else {
        return None;
    };
    if let H::TypeName_::ModuleType(_, struct_name) = tname.value {
        return Some(struct_name);
    }
    None
}

fn value_evaluation_order(
    context: &mut Context,
    block: &mut Block,
    input_exps: Vec<(T::Exp, Option<H::Type>)>,
) -> Vec<H::Exp> {
    let mut needs_binding = false;
    let mut statements = vec![];
    let mut values = vec![];
    for (exp, expected_type) in input_exps.into_iter().rev() {
        let te_loc = exp.exp.loc;
        let mut new_stmts = make_block!();
        let exp = value(context, &mut new_stmts, expected_type.as_ref(), exp);
        // If evaluating this expression introduces statements, all previous exps need to be bound
        // to preserve left-to-right evaluation order
        let e = if needs_binding {
            maybe_bind_exp(context, &mut new_stmts, exp)
        } else {
            exp
        };
        if let Some(final_exp) = e {
            values.push(final_exp);
        } else {
            values.push(unit_exp(te_loc));
        }
        let adds_to_result = !new_stmts.is_empty();
        needs_binding = needs_binding || adds_to_result;
        statements.push(new_stmts);
    }
    block.append(&mut statements.into_iter().rev().flatten().collect());
    values.into_iter().rev().collect()
}

fn maybe_bind_exp(context: &mut Context, stmts: &mut Block, e: Option<H::Exp>) -> Option<H::Exp> {
    if let Some(e) = e {
        let loc = e.exp.loc;
        let ty = e.ty.clone();
        let (binders, var_exp) = make_binders(context, loc, ty.clone());
        if binders.is_empty() {
            make_ignore_and_pop(stmts, Some(e));
            None
        } else {
            bind_value_in_block(context, binders, Some(ty), stmts, Some(e));
            Some(var_exp)
        }
    } else {
        e
    }
}

fn bind_exp(context: &mut Context, stmts: &mut Block, e: H::Exp) -> H::Exp {
    let loc = e.exp.loc;
    let ty = e.ty.clone();
    let (binders, var_exp) = make_binders(context, loc, ty.clone());
    bind_value_in_block(context, binders, Some(ty), stmts, Some(e));
    var_exp
}

// Takes binder(s), a block, and a value. If the value is defined, adds an assignment to the end
// of the block to assign the binders to that value.
// Returns the block and a flag indicating if that operation happened.
fn bind_value_in_block(
    context: &mut Context,
    binders: Vec<H::LValue>,
    binders_type: Option<H::Type>,
    stmts: &mut Block,
    value_exp: Option<H::Exp>,
) -> bool {
    use H::{Command_ as C, Statement_ as S};
    for sp!(_, lvalue) in &binders {
        match lvalue {
            H::LValue_::Var(_, _) => (),
            _ => panic!("ICE tried bind_value for non-var lvalue"),
        }
    }
    let rhs_exp = maybe_freeze(context, stmts, binders_type, value_exp);
    if let Some(real_exp) = rhs_exp {
        let loc = real_exp.exp.loc;
        stmts.push_back(sp(loc, S::Command(sp(loc, C::Assign(binders, real_exp)))));
        true
    } else {
        false
    }
}

fn make_binders(context: &mut Context, loc: Loc, ty: H::Type) -> (Vec<H::LValue>, H::Exp) {
    use H::Type_ as T;
    use H::UnannotatedExp_ as E;
    match ty.value {
        T::Unit => (
            vec![],
            H::exp(
                tunit(loc),
                sp(
                    loc,
                    E::Unit {
                        case: H::UnitCase::Implicit,
                    },
                ),
            ),
        ),
        T::Single(single_type) => {
            let (binder, var_exp) = make_temp(context, loc, single_type);
            (vec![binder], var_exp)
        }
        T::Multiple(types) => {
            let (binders, vars) = types
                .iter()
                .map(|single_type| make_temp(context, loc, single_type.clone()))
                .unzip();
            (
                binders,
                H::exp(
                    sp(loc, T::Multiple(types)),
                    sp(loc, H::UnannotatedExp_::Multiple(vars)),
                ),
            )
        }
    }
}

fn make_temp(context: &mut Context, loc: Loc, sp!(_, ty): H::SingleType) -> (H::LValue, H::Exp) {
    let binder = context.new_temp(loc, sp(loc, ty.clone()));
    let lvalue = sp(loc, H::LValue_::Var(binder, Box::new(sp(loc, ty.clone()))));
    let uexp = sp(
        loc,
        H::UnannotatedExp_::Move {
            annotation: MoveOpAnnotation::InferredLastUsage,
            var: binder,
        },
    );
    (lvalue, H::exp(H::Type_::single(sp(loc, ty)), uexp))
}

fn builtin(
    context: &mut Context,
    block: &mut Block,
    _eloc: Loc,
    sp!(loc, tb_): T::BuiltinFunction,
    targ: Box<T::Exp>,
) -> Option<H::UnannotatedExp_> {
    use H::{BuiltinFunction_ as HB, UnannotatedExp_ as E};
    use T::BuiltinFunction_ as TB;

    macro_rules! maybe_output {
        ($arg0: expr, $args:expr) => {
            if let Some(args) = $args {
                Some(E::Builtin(Box::new(sp(loc, $arg0)), args))
            } else {
                None
            }
        };
    }

    match tb_ {
        TB::MoveTo(bt) => {
            let texpected_tys = vec![
                sp(loc, N::Type_::Ref(false, Box::new(N::Type_::signer(loc)))),
                bt.clone(),
            ];
            let texpected_ty_ = N::Type_::Apply(
                Some(AbilitySet::empty()), // Should be unused
                sp(loc, N::TypeName_::Multiple(texpected_tys.len())),
                texpected_tys,
            );
            let expected_ty = type_(context, sp(loc, texpected_ty_));
            let args = value_list(context, block, Some(&expected_ty), *targ);
            let ty = base_type(context, bt);
            maybe_output!(HB::MoveTo(ty), args)
        }
        TB::MoveFrom(bt) => {
            let ty = base_type(context, bt);
            let args = value_list(context, block, None, *targ);
            maybe_output!(HB::MoveFrom(ty), args)
        }
        TB::BorrowGlobal(mut_, bt) => {
            let ty = base_type(context, bt);
            let args = value_list(context, block, None, *targ);
            maybe_output!(HB::BorrowGlobal(mut_, ty), args)
        }
        TB::Exists(bt) => {
            let ty = base_type(context, bt);
            let args = value_list(context, block, None, *targ);
            maybe_output!(HB::Exists(ty), args)
        }
        TB::Freeze(_bt) => {
            let args = value(context, block, None, *targ);
            args.map(|arg| E::Freeze(Box::new(arg)))
        }
        TB::Assert(_) => unreachable!(),
    }
}

fn process_value(sp!(loc, ev_): E::Value) -> H::Value {
    use E::Value_ as EV;
    use H::Value_ as HV;
    let v_ = match ev_ {
        EV::InferredNum(_) => panic!("ICE should have been expanded"),
        EV::Address(a) => HV::Address(a.into_addr_bytes()),
        EV::U8(u) => HV::U8(u),
        EV::U16(u) => HV::U16(u),
        EV::U32(u) => HV::U32(u),
        EV::U64(u) => HV::U64(u),
        EV::U128(u) => HV::U128(u),
        EV::U256(u) => HV::U256(u),
        EV::Bool(u) => HV::Bool(u),
        EV::Bytearray(bytes) => HV::Vector(
            Box::new(H::BaseType_::u8(loc)),
            bytes.into_iter().map(|b| sp(loc, HV::U8(b))).collect(),
        ),
    };
    sp(loc, v_)
}

fn process_binops(
    context: &mut Context,
    input_block: &mut Block,
    result_type: H::Type,
    e: T::Exp,
) -> Option<H::Exp> {
    use T::UnannotatedExp_ as E;

    enum Pn {
        Op(BinOp, H::Type, Loc),
        Val(Block, Option<H::Exp>),
    }

    // ----------------------------------------
    // Convert nested binops into a PN list

    let mut pn_stack = vec![];

    let mut work_queue = vec![(e, result_type)];

    while let Some((exp, ty)) = work_queue.pop() {
        if let T::Exp {
            exp: sp!(eloc, E::BinopExp(lhs, op, op_type, rhs)),
            ..
        } = exp
        {
            pn_stack.push(Pn::Op(op, ty, eloc));
            let op_type = freeze_ty(type_(context, *op_type));
            // push on backwards so when we reverse the stack, we are in RPN order
            work_queue.push((*rhs, op_type.clone()));
            work_queue.push((*lhs, op_type));
        } else {
            let mut exp_block = make_block!();
            let exp = value(context, &mut exp_block, Some(ty).as_ref(), exp);
            pn_stack.push(Pn::Val(exp_block, exp));
        }
    }

    // ----------------------------------------
    // Now process as an RPN stack

    let mut value_stack: Vec<(Block, Option<H::Exp>)> = vec![];

    for entry in pn_stack.into_iter().rev() {
        match entry {
            Pn::Op(sp!(loc, op @ BinOp_::And), ty, eloc) => {
                let test = value_stack.pop().expect("ICE binop hlir issue");
                let if_ = value_stack.pop().expect("ICE binop hlir issue");
                if test.1.is_some() && simple_bool_binop_arg(&if_) {
                    let (mut test_block, test_exp) = test;
                    let (mut if_block, if_exp) = if_;
                    test_block.append(&mut if_block);
                    let exp = maybe_make_binop(test_exp, sp(loc, op), if_exp)
                        .map(|e| H::exp(ty, sp(eloc, e)));
                    value_stack.push((test_block, exp));
                } else {
                    let else_ = (make_block!(), Some(bool_exp(loc, false)));
                    value_stack.push(make_boolean_binop(context, sp(loc, op), test, if_, else_));
                }
            }
            Pn::Op(sp!(loc, op @ BinOp_::Or), ty, eloc) => {
                let test = value_stack.pop().expect("ICE binop hlir issue");
                let else_ = value_stack.pop().expect("ICE binop hlir issue");
                if test.1.is_some() && simple_bool_binop_arg(&else_) {
                    let (mut test_block, test_exp) = test;
                    let (mut else_block, else_exp) = else_;
                    test_block.append(&mut else_block);
                    let exp = maybe_make_binop(test_exp, sp(loc, op), else_exp)
                        .map(|e| H::exp(ty, sp(eloc, e)));
                    value_stack.push((test_block, exp));
                } else {
                    let if_ = (make_block!(), Some(bool_exp(loc, true)));
                    value_stack.push(make_boolean_binop(context, sp(loc, op), test, if_, else_));
                }
            }
            Pn::Op(op, ty, loc) => {
                let (mut lhs_block, lhs_exp) = value_stack.pop().expect("ICE binop hlir issue");
                let (mut rhs_block, rhs_exp) = value_stack.pop().expect("ICE binop hlir issue");
                lhs_block.append(&mut rhs_block);
                // nb: here we could check if the LHS and RHS are "large" terms and let-bind them
                // if they are getting too big.
                let exp = maybe_make_binop(lhs_exp, op, rhs_exp).map(|e| H::exp(ty, sp(loc, e)));
                value_stack.push((lhs_block, exp));
            }
            Pn::Val(block, exp) => value_stack.push((block, exp)),
        }
    }
    assert!(value_stack.len() == 1, "ICE binop hlir stack unprocessed");
    let (mut final_block, final_exp) = value_stack.pop().unwrap();
    input_block.append(&mut final_block);
    final_exp
}

fn maybe_make_binop(
    lhs: Option<H::Exp>,
    op: BinOp,
    rhs: Option<H::Exp>,
) -> Option<H::UnannotatedExp_> {
    if let (Some(lhs), Some(rhs)) = (lhs, rhs) {
        Some(H::UnannotatedExp_::BinopExp(
            Box::new(lhs),
            op,
            Box::new(rhs),
        ))
    } else {
        None
    }
}

fn make_boolean_binop(
    context: &mut Context,
    op: BinOp,
    (mut test_block, test_exp): (Block, Option<H::Exp>),
    (mut if_block, if_exp): (Block, Option<H::Exp>),
    (mut else_block, else_exp): (Block, Option<H::Exp>),
) -> (Block, Option<H::Exp>) {
    let loc = op.loc;

    let bool_ty = tbool(loc);
    let (binders, bound_exp) = make_binders(context, loc, bool_ty.clone());
    let opty = Some(bool_ty);

    // one of these _must_ case always binds by construction.
    let if_bind = bind_value_in_block(
        context,
        binders.clone(),
        opty.clone(),
        &mut if_block,
        if_exp,
    );
    let else_bind = bind_value_in_block(context, binders, opty, &mut else_block, else_exp);
    assert!(if_bind || else_bind, "ICE boolean binop processing failure");

    if let Some(cond) = test_exp {
        let if_else = H::Statement_::IfElse {
            cond: Box::new(cond),
            if_block,
            else_block,
        };
        test_block.push_back(sp(loc, if_else));
        let final_exp = Some(bound_exp);
        (test_block, final_exp)
    } else {
        (test_block, None)
    }
}

fn simple_bool_binop_arg((block, exp): &(Block, Option<H::Exp>)) -> bool {
    use H::UnannotatedExp_ as HE;
    if !block.is_empty() {
        false
    } else if let Some(exp) = exp {
        matches!(
            exp.exp.value,
            HE::Value(_)
                | HE::Constant(_)
                | HE::Move { .. }
                | HE::Copy { .. }
                | HE::UnresolvedError
        )
    } else {
        false
    }
}

//**************************************************************************************************
// Freezing
//**************************************************************************************************

#[derive(PartialEq, Eq)]
enum Freeze {
    NotNeeded,
    Point,
    Sub(Vec<bool>),
}

fn needs_freeze(context: &Context, sp!(_, actual): &H::Type, sp!(_, expected): &H::Type) -> Freeze {
    use H::Type_ as T;
    match (actual, expected) {
        (T::Unit, T::Unit) => Freeze::NotNeeded,
        (T::Single(actual_type), T::Single(expected_type)) => {
            if needs_freeze_single(actual_type, expected_type) {
                Freeze::Point
            } else {
                Freeze::NotNeeded
            }
        }
        (T::Multiple(actual_ss), T::Multiple(actual_es)) => {
            assert!(actual_ss.len() == actual_es.len());
            let points = actual_ss
                .iter()
                .zip(actual_es)
                .map(|(a, e)| needs_freeze_single(a, e))
                .collect::<Vec<_>>();
            if points.iter().any(|needs| *needs) {
                Freeze::Sub(points)
            } else {
                Freeze::NotNeeded
            }
        }
        (_actual, _expected) => {
            assert!(context.env.has_errors());
            Freeze::NotNeeded
        }
    }
}

fn needs_freeze_single(sp!(_, actual): &H::SingleType, sp!(_, expected): &H::SingleType) -> bool {
    use H::SingleType_ as T;
    matches!((actual, expected), (T::Ref(true, _), T::Ref(false, _)))
}

fn freeze(context: &mut Context, expected_type: &H::Type, e: H::Exp) -> (Block, H::Exp) {
    use H::{Type_ as T, UnannotatedExp_ as E};

    match needs_freeze(context, &e.ty, expected_type) {
        Freeze::NotNeeded => (make_block!(), e),
        Freeze::Point => (make_block!(), freeze_point(e)),
        Freeze::Sub(points) => {
            let mut bind_stmts = make_block!();
            let bound_rhs = bind_exp(context, &mut bind_stmts, e);
            if let H::Exp {
                ty: _,
                exp: sp!(eloc, E::Multiple(exps)),
            } = bound_rhs
            {
                assert!(exps.len() == points.len());
                let exps: Vec<_> = exps
                    .into_iter()
                    .zip(points)
                    .map(|(exp, needs_freeze)| if needs_freeze { freeze_point(exp) } else { exp })
                    .collect();
                let tys = exps
                    .iter()
                    .map(|e| match &e.ty.value {
                        T::Single(s) => s.clone(),
                        _ => panic!("ICE list item has Multple type"),
                    })
                    .collect();
                (
                    bind_stmts,
                    H::exp(sp(eloc, T::Multiple(tys)), sp(eloc, E::Multiple(exps))),
                )
            } else {
                unreachable!("ICE needs_freeze failed")
            }
        }
    }
}

fn freeze_point(e: H::Exp) -> H::Exp {
    let frozen_ty = freeze_ty(e.ty.clone());
    let eloc = e.exp.loc;
    let e_ = H::UnannotatedExp_::Freeze(Box::new(e));
    H::exp(frozen_ty, sp(eloc, e_))
}

fn freeze_ty(sp!(tloc, t): H::Type) -> H::Type {
    use H::Type_ as T;
    match t {
        T::Single(s) => sp(tloc, T::Single(freeze_single(s))),
        t => sp(tloc, t),
    }
}

fn freeze_single(sp!(sloc, s): H::SingleType) -> H::SingleType {
    use H::SingleType_ as S;
    match s {
        S::Ref(true, inner) => sp(sloc, S::Ref(false, inner)),
        s => sp(sloc, s),
    }
}

//**************************************************************************************************
// Generates warnings for unused struct fields.
//**************************************************************************************************

fn gen_unused_warnings(
    context: &mut Context,
    is_source_module: bool,
    structs: &UniqueMap<StructName, H::StructDefinition>,
) {
    if !is_source_module {
        // generate warnings only for modules compiled in this pass rather than for all modules
        // including pre-compiled libraries for which we do not have source code available and
        // cannot be analyzed in this pass
        return;
    }

    for (_, sname, sdef) in structs {
        context
            .env
            .add_warning_filter_scope(sdef.warning_filter.clone());

        if let H::StructFields::Defined(fields) = &sdef.fields {
            for (f, _) in fields {
                if !context
                    .used_fields
                    .get(sname)
                    .is_some_and(|names| names.contains(&f.value()))
                {
                    let msg = format!("The '{}' field of the '{sname}' type is unused", f.value());
                    context
                        .env
                        .add_diag(diag!(UnusedItem::StructField, (f.loc(), msg)));
                }
            }
        }

        context.env.pop_warning_filter_scope();
    }
}
