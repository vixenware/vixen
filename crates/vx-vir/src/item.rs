//! Top-level declaration items.
//!
//! Items are the top-level declarations in a source file or namespace.
//! They also appear as statement-level declarations inside blocks.

use crate::{BodyId, ExprId, ItemId, ScopeId, TypeRefId};
use vx_resolve::{DefId, SourceSpan};
use vx_types::TypeId;

/// A top-level declaration item.
#[derive(Debug, Clone)]
pub struct Item {
    /// Source span of the entire declaration.
    /// Populated during: CST walk.
    pub span: SourceSpan,

    /// The DefId from the resolve pass. Links back to scope tree.
    /// Populated during: CST walk (carried over from scope tree).
    pub def_id: DefId,

    /// The declaration kind.
    /// Populated during: CST walk.
    pub kind: ItemKind,
}

/// Discriminant for all declaration item forms.
#[derive(Debug, Clone)]
pub enum ItemKind {
    /// A function declaration: `fn name(params) ( body )`.
    Function(FunctionItem),

    /// A struct declaration: `struct Name { fields }`.
    Struct(StructItem),

    /// An enum declaration: `enum Name { variants }`.
    Enum(EnumItem),

    /// A newtype declaration: `newtype Name of T`.
    Newtype(NewtypeItem),

    /// A type alias: `type Name = T`.
    TypeAlias(TypeAliasItem),

    /// A namespace: `namespace Name { items }`.
    Namespace(NamespaceItem),

    /// A malformed item node that could not be parsed. Preserves the CST node's
    /// position in the item list so later passes can report diagnostics.
    Error,
}

/// A function declaration item.
#[derive(Debug, Clone)]
pub struct FunctionItem {
    /// The function name.
    pub name: String,
    /// Span of the name token.
    pub name_span: SourceSpan,
    /// The body of the function.
    pub body: BodyId,
    /// The TypeId assigned during type collection, if applicable.
    /// Populated during: CST walk (from type collection results).
    pub type_id: Option<TypeId>,
    /// Attributes on this function.
    pub attributes: Vec<Attribute>,
}

/// A field in a struct or enum variant declaration.
#[derive(Debug, Clone)]
pub struct FieldDecl {
    /// The field name.
    pub name: String,
    /// Span of the field name token.
    pub name_span: SourceSpan,
    /// The declared type of this field.
    pub ty: Option<TypeRefId>,
    /// Whether this field has a default value.
    pub has_default: bool,
    /// The default value expression, if any.
    pub default_value: Option<ExprId>,
    /// Attributes on this field.
    pub attributes: Vec<Attribute>,
}

/// A struct declaration item.
#[derive(Debug, Clone)]
pub struct StructItem {
    /// The struct name.
    pub name: String,
    /// Span of the name token.
    pub name_span: SourceSpan,
    /// The TypeId assigned during type collection.
    pub type_id: Option<TypeId>,
    /// Type parameter names (e.g. `["T", "U"]`).
    pub type_params: Vec<String>,
    /// The struct fields.
    pub fields: Vec<FieldDecl>,
    /// Attributes on this struct.
    pub attributes: Vec<Attribute>,
}

/// A variant in an enum declaration.
#[derive(Debug, Clone)]
pub struct VariantDecl {
    /// The variant name.
    pub name: String,
    /// Span of the variant name token.
    pub name_span: SourceSpan,
    /// The variant's fields (may be empty for unit variants).
    pub fields: Vec<FieldDecl>,
    /// Attributes on this variant.
    pub attributes: Vec<Attribute>,
}

/// An enum declaration item.
#[derive(Debug, Clone)]
pub struct EnumItem {
    /// The enum name.
    pub name: String,
    /// Span of the name token.
    pub name_span: SourceSpan,
    /// The TypeId assigned during type collection.
    pub type_id: Option<TypeId>,
    /// Type parameter names.
    pub type_params: Vec<String>,
    /// The enum variants.
    pub variants: Vec<VariantDecl>,
    /// Attributes on this enum.
    pub attributes: Vec<Attribute>,
}

/// A newtype declaration item.
#[derive(Debug, Clone)]
pub struct NewtypeItem {
    /// The newtype name.
    pub name: String,
    /// Span of the name token.
    pub name_span: SourceSpan,
    /// The TypeId assigned during type collection.
    pub type_id: Option<TypeId>,
    /// Type parameter names.
    pub type_params: Vec<String>,
    /// The inner type.
    pub inner_ty: Option<TypeRefId>,
    /// Attributes on this newtype.
    pub attributes: Vec<Attribute>,
}

/// A type alias declaration item.
#[derive(Debug, Clone)]
pub struct TypeAliasItem {
    /// The type alias name.
    pub name: String,
    /// Span of the name token.
    pub name_span: SourceSpan,
    /// The TypeId assigned during type collection.
    pub type_id: Option<TypeId>,
    /// Type parameter names.
    pub type_params: Vec<String>,
    /// The aliased type.
    pub aliased_ty: Option<TypeRefId>,
    /// Attributes on this type alias.
    pub attributes: Vec<Attribute>,
}

/// A namespace declaration item.
#[derive(Debug, Clone)]
pub struct NamespaceItem {
    /// The namespace name.
    pub name: String,
    /// Span of the name token.
    pub name_span: SourceSpan,
    /// The scope introduced by this namespace.
    /// Populated during: CST walk.
    pub scope: ScopeId,
    /// The contained items (for `namespace Name { ... }` form).
    pub items: Vec<ItemId>,
    /// Attributes on this namespace.
    pub attributes: Vec<Attribute>,
}

/// An attribute on a declaration: `#[key: value]`.
#[derive(Debug, Clone)]
pub struct Attribute {
    /// The attribute entries.
    pub entries: Vec<AttributeEntry>,
    /// Span of the entire attribute.
    pub span: SourceSpan,
}

/// A single entry in an attribute: `key` or `key: value`.
#[derive(Debug, Clone)]
pub struct AttributeEntry {
    /// The attribute key.
    pub key: String,
    /// Span of the key token.
    pub key_span: SourceSpan,
    /// The attribute value expression, if any.
    pub value: Option<ExprId>,
}
