use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayoutId(pub u32);

impl LayoutId {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KernelId(pub u32);

impl KernelId {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalId(pub u32);

impl LocalId {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockId(pub u32);

impl BlockId {
    pub const fn new(id: u32) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldKey {
    Index(usize),
    Name(String),
}

impl FieldKey {
    pub fn index(index: usize) -> Self {
        Self::Index(index)
    }
}

impl fmt::Display for FieldKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(index) => write!(f, "{index}"),
            Self::Name(name) => write!(f, "{name}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallableSig {
    pub params: Vec<Type>,
    pub ret: Box<Type>,
}

impl CallableSig {
    pub fn new(params: Vec<Type>, ret: Type) -> Self {
        Self {
            params,
            ret: Box::new(ret),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    RuntimeCtx,
    ThunkState,
    U64,
    Bool,
    Ref(Box<Type>),
    Obs(Box<Type>),
    Callable(CallableSig),
    ProductHandle(LayoutId),
    ClosureHandle(CallableSig),
    EnvHandle(LayoutId),
    ThunkHandle(Box<Type>),
}

impl Type {
    pub fn ref_of(inner: Type) -> Self {
        Self::Ref(Box::new(inner))
    }

    pub fn obs_of(inner: Type) -> Self {
        Self::Obs(Box::new(inner))
    }

    pub fn thunk_handle(inner: Type) -> Self {
        Self::ThunkHandle(Box::new(inner))
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeCtx => write!(f, "RuntimeCtx"),
            Self::ThunkState => write!(f, "ThunkState"),
            Self::U64 => write!(f, "U64"),
            Self::Bool => write!(f, "Bool"),
            Self::Ref(inner) => write!(f, "Ref<{inner}>"),
            Self::Obs(inner) => write!(f, "Obs<{inner}>"),
            Self::Callable(sig) => write!(f, "Callable<{sig}>"),
            Self::ProductHandle(layout) => write!(f, "ProductHandle#{}", layout.0),
            Self::ClosureHandle(sig) => write!(f, "ClosureHandle<{sig}>"),
            Self::EnvHandle(layout) => write!(f, "EnvHandle#{}", layout.0),
            Self::ThunkHandle(inner) => write!(f, "ThunkHandle<{inner}>"),
        }
    }
}

impl fmt::Display for CallableSig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "(")?;
        for (index, param) in self.params.iter().enumerate() {
            if index > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{param}")?;
        }
        write!(f, ") -> {}", self.ret)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObsLiteral {
    NeedValue,
    NeedCallable,
    NeedProduct(Vec<(FieldKey, ObsLiteral)>),
}

impl fmt::Display for ObsLiteral {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedValue => write!(f, "NeedValue"),
            Self::NeedCallable => write!(f, "NeedCallable"),
            Self::NeedProduct(fields) => {
                write!(f, "NeedProduct {{ ")?;
                for (index, (field, obs)) in fields.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{field}: {obs}")?;
                }
                write!(f, " }}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedField {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductField {
    pub key: FieldKey,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutKind {
    Product { fields: Vec<ProductField> },
    Env { fields: Vec<NamedField> },
    ThunkFrame { fields: Vec<NamedField> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub id: LayoutId,
    pub name: String,
    pub kind: LayoutKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDecl {
    pub id: LocalId,
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnType {
    Never,
    Strict(Type),
    Ref(Type),
}

impl fmt::Display for ReturnType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Never => write!(f, "Never"),
            Self::Strict(ty) => write!(f, "{ty}"),
            Self::Ref(ty) => write!(f, "Ref<{ty}>"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    ConstU64 {
        dst: LocalId,
        value: u64,
    },
    ConstBool {
        dst: LocalId,
        value: bool,
    },
    AddU64 {
        dst: LocalId,
        lhs: LocalId,
        rhs: LocalId,
    },
    Copy {
        dst: LocalId,
        src: LocalId,
    },
    ReadyU64 {
        dst: LocalId,
        value: LocalId,
    },
    ReadyBool {
        dst: LocalId,
        value: LocalId,
    },
    ReadyCallable {
        dst: LocalId,
        callable: LocalId,
    },
    AllocEnv {
        dst: LocalId,
        layout: LayoutId,
        captures: Vec<(String, LocalId)>,
    },
    AllocClosure {
        dst: LocalId,
        kernel: KernelId,
        env: Option<LocalId>,
    },
    AllocThunk {
        dst: LocalId,
        kernel: KernelId,
        frame_layout: LayoutId,
        frame: Vec<(String, LocalId)>,
    },
    AllocProduct {
        dst: LocalId,
        layout: LayoutId,
        fields: Vec<(FieldKey, LocalId)>,
    },
    EnterU64 {
        dst: LocalId,
        reference: LocalId,
        obs: ObsLiteral,
    },
    EnterBool {
        dst: LocalId,
        reference: LocalId,
        obs: ObsLiteral,
    },
    NeedCallable {
        dst: LocalId,
        reference: LocalId,
    },
    ProjectField {
        dst: LocalId,
        product: LocalId,
        field: FieldKey,
    },
    EnvGetCapture {
        dst: LocalId,
        env: LocalId,
        capture: String,
    },
    ThunkState {
        dst: LocalId,
        thunk: LocalId,
    },
    StateIsReady {
        dst: LocalId,
        state: LocalId,
    },
    LoadCachedU64 {
        dst: LocalId,
        thunk: LocalId,
    },
    MarkEvaluating {
        thunk: LocalId,
    },
    CacheReadyU64 {
        thunk: LocalId,
        value: LocalId,
    },
    CacheReadyProduct {
        thunk: LocalId,
        product: LocalId,
    },
    CallCallable {
        callable: LocalId,
        self_thunk: LocalId,
        args: Vec<LocalId>,
        obs: ObsLiteral,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Goto(BlockId),
    Branch {
        cond: LocalId,
        then_block: BlockId,
        else_block: BlockId,
    },
    ReturnStrict(LocalId),
    ReturnRef(LocalId),
    Done,
    Trap {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub stmts: Vec<Stmt>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kernel {
    pub id: KernelId,
    pub name: String,
    pub params: Vec<LocalDecl>,
    pub locals: Vec<LocalDecl>,
    pub return_type: ReturnType,
    pub entry: BlockId,
    pub blocks: Vec<Block>,
}

impl Kernel {
    pub fn local_name(&self, local: LocalId) -> &str {
        self.params
            .iter()
            .chain(self.locals.iter())
            .find(|decl| decl.id == local)
            .map(|decl| decl.name.as_str())
            .unwrap_or("<unknown>")
    }

    pub fn local_decl(&self, local: LocalId) -> Option<&LocalDecl> {
        self.params
            .iter()
            .chain(self.locals.iter())
            .find(|decl| decl.id == local)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Module {
    pub layouts: Vec<Layout>,
    pub kernels: Vec<Kernel>,
}

impl Module {
    pub fn kernel_named(&self, name: &str) -> Option<&Kernel> {
        self.kernels.iter().find(|kernel| kernel.name == name)
    }
}

impl fmt::Display for Module {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.layouts.is_empty() {
            writeln!(f, "layouts:")?;
            for layout in &self.layouts {
                writeln!(f, "{layout}")?;
            }
            writeln!(f)?;
        }

        for (index, kernel) in self.kernels.iter().enumerate() {
            if index > 0 {
                writeln!(f)?;
            }
            fmt_kernel(f, kernel)?;
        }

        Ok(())
    }
}

impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "  layout #{} {} = ", self.id.0, self.name)?;
        match &self.kind {
            LayoutKind::Product { fields } => {
                write!(f, "product {{ ")?;
                for (index, field) in fields.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", field.key, field.ty)?;
                }
                write!(f, " }}")
            }
            LayoutKind::Env { fields } => {
                write!(f, "env {{ ")?;
                for (index, field) in fields.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", field.name, field.ty)?;
                }
                write!(f, " }}")
            }
            LayoutKind::ThunkFrame { fields } => {
                write!(f, "thunk_frame {{ ")?;
                for (index, field) in fields.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", field.name, field.ty)?;
                }
                write!(f, " }}")
            }
        }
    }
}

fn fmt_kernel(f: &mut fmt::Formatter<'_>, kernel: &Kernel) -> fmt::Result {
    write!(f, "kernel #{} {}(", kernel.id.0, kernel.name)?;
    for (index, param) in kernel.params.iter().enumerate() {
        if index > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{}: {}", param.name, param.ty)?;
    }
    writeln!(f, ") -> {} {{", kernel.return_type)?;

    if !kernel.locals.is_empty() {
        writeln!(f, "  locals:")?;
        for local in &kernel.locals {
            writeln!(f, "    {}: {}", local.name, local.ty)?;
        }
    }

    for block in &kernel.blocks {
        writeln!(f, "  bb{}:", block.id.0)?;
        for stmt in &block.stmts {
            writeln!(f, "    {}", stmt.display(kernel))?;
        }
        writeln!(f, "    {}", block.terminator.display(kernel))?;
    }

    writeln!(f, "}}")
}

impl Stmt {
    fn display<'a>(&'a self, kernel: &'a Kernel) -> StmtDisplay<'a> {
        StmtDisplay { stmt: self, kernel }
    }
}

struct StmtDisplay<'a> {
    stmt: &'a Stmt,
    kernel: &'a Kernel,
}

impl fmt::Display for StmtDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = |local| self.kernel.local_name(local);
        match self.stmt {
            Stmt::ConstU64 { dst, value } => write!(f, "{} = const_u64 {}", name(*dst), value),
            Stmt::ConstBool { dst, value } => write!(f, "{} = const_bool {}", name(*dst), value),
            Stmt::AddU64 { dst, lhs, rhs } => {
                write!(f, "{} = add_u64 {}, {}", name(*dst), name(*lhs), name(*rhs))
            }
            Stmt::Copy { dst, src } => write!(f, "{} = copy {}", name(*dst), name(*src)),
            Stmt::ReadyU64 { dst, value } => {
                write!(f, "{} = ready_u64 {}", name(*dst), name(*value))
            }
            Stmt::ReadyBool { dst, value } => {
                write!(f, "{} = ready_bool {}", name(*dst), name(*value))
            }
            Stmt::ReadyCallable { dst, callable } => {
                write!(f, "{} = ready_callable {}", name(*dst), name(*callable))
            }
            Stmt::AllocEnv {
                dst,
                layout,
                captures,
            } => {
                write!(f, "{} = alloc_env #{} {{ ", name(*dst), layout.0)?;
                for (index, (capture, local)) in captures.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{capture}: {}", name(*local))?;
                }
                write!(f, " }}")
            }
            Stmt::AllocClosure { dst, kernel, env } => {
                write!(f, "{} = alloc_closure kernel#{}", name(*dst), kernel.0)?;
                if let Some(env) = env {
                    write!(f, " {}", name(*env))?;
                }
                Ok(())
            }
            Stmt::AllocThunk {
                dst,
                kernel,
                frame_layout,
                frame,
            } => {
                write!(
                    f,
                    "{} = alloc_thunk kernel#{} frame#{} {{ ",
                    name(*dst),
                    kernel.0,
                    frame_layout.0
                )?;
                for (index, (field, local)) in frame.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{field}: {}", name(*local))?;
                }
                write!(f, " }}")
            }
            Stmt::AllocProduct {
                dst,
                layout,
                fields,
            } => {
                write!(f, "{} = alloc_product #{} {{ ", name(*dst), layout.0)?;
                for (index, (field, local)) in fields.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{field}: {}", name(*local))?;
                }
                write!(f, " }}")
            }
            Stmt::EnterU64 {
                dst,
                reference,
                obs,
            } => write!(
                f,
                "{} = enter_u64 {}, {}",
                name(*dst),
                name(*reference),
                obs
            ),
            Stmt::EnterBool {
                dst,
                reference,
                obs,
            } => write!(
                f,
                "{} = enter_bool {}, {}",
                name(*dst),
                name(*reference),
                obs
            ),
            Stmt::NeedCallable { dst, reference } => {
                write!(f, "{} = need_callable {}", name(*dst), name(*reference))
            }
            Stmt::ProjectField {
                dst,
                product,
                field,
            } => {
                write!(f, "{} = project {}.{}", name(*dst), name(*product), field)
            }
            Stmt::EnvGetCapture { dst, env, capture } => {
                write!(f, "{} = env_get {}.{}", name(*dst), name(*env), capture)
            }
            Stmt::ThunkState { dst, thunk } => {
                write!(f, "{} = thunk_state {}", name(*dst), name(*thunk))
            }
            Stmt::StateIsReady { dst, state } => {
                write!(f, "{} = state_is_ready {}", name(*dst), name(*state))
            }
            Stmt::LoadCachedU64 { dst, thunk } => {
                write!(f, "{} = load_cached_u64 {}", name(*dst), name(*thunk))
            }
            Stmt::MarkEvaluating { thunk } => write!(f, "mark_evaluating {}", name(*thunk)),
            Stmt::CacheReadyU64 { thunk, value } => {
                write!(f, "cache_ready_u64 {}, {}", name(*thunk), name(*value))
            }
            Stmt::CacheReadyProduct { thunk, product } => {
                write!(
                    f,
                    "cache_ready_product {}, {}",
                    name(*thunk),
                    name(*product)
                )
            }
            Stmt::CallCallable {
                callable,
                self_thunk,
                args,
                obs,
            } => {
                write!(
                    f,
                    "call_callable {}, {}, [",
                    name(*callable),
                    name(*self_thunk)
                )?;
                for (index, arg) in args.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", name(*arg))?;
                }
                write!(f, "], {obs}")
            }
        }
    }
}

impl Terminator {
    fn display<'a>(&'a self, kernel: &'a Kernel) -> TerminatorDisplay<'a> {
        TerminatorDisplay {
            terminator: self,
            kernel,
        }
    }
}

struct TerminatorDisplay<'a> {
    terminator: &'a Terminator,
    kernel: &'a Kernel,
}

impl fmt::Display for TerminatorDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = |local| self.kernel.local_name(local);
        match self.terminator {
            Terminator::Goto(block) => write!(f, "goto bb{}", block.0),
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => write!(
                f,
                "if {} goto bb{} else bb{}",
                name(*cond),
                then_block.0,
                else_block.0
            ),
            Terminator::ReturnStrict(local) => write!(f, "return_strict {}", name(*local)),
            Terminator::ReturnRef(local) => write!(f, "return_ref {}", name(*local)),
            Terminator::Done => write!(f, "done"),
            Terminator::Trap { message } => write!(f, "trap {message:?}"),
        }
    }
}
