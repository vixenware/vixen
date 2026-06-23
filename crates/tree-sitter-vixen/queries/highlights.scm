[
  "fn"
  "let"
  "type"
  "struct"
  "enum"
  "newtype"
  "of"
  "with"
  "where"
  "match"
  "if"
  "else"
  "is"
  "as"
  "as?"
  "import"
  "return"
  "fail"
  "namespace"
  "extend"
] @keyword

[
  "true"
  "false"
] @boolean

[
  "{"
  "}"
  "("
  ")"
  "["
  "]"
] @punctuation.bracket

[
  ","
  ";"
  "."
  ":"
  "::"
  "=>"
  "->"
  "="
  "with="
  "?"
  "!"
  "#"
  "#["
] @punctuation.delimiter

[
  "+"
  "-"
  "*"
  "/"
  "%"
  "=="
  "!="
  "<"
  "<="
  ">"
  ">="
  "&&"
  "||"
  "|>"
  ".."
  "..="
] @operator

(doc_comment) @comment.doc
(line_comment) @comment
(block_comment) @comment
(escape_sequence) @string.escape
(string_literal) @string
(interpolated_string) @string.special
(path_literal) @string.special
(interpolated_path_literal) @string.special
(path_content) @string.special
(string_content) @string
(integer_literal) @number
(float_literal) @number.float
(dollar_identifier) @variable.special
(pipe_placeholder) @variable.special

(attribute_entry
  key: (lower_identifier) @attribute)

(capability_tag
  "#" @operator
  (lower_identifier) @label)

(function_declaration
  (lower_identifier) @function)

(function_declaration
  (upper_identifier) @constructor)

(parameter
  (binding_pattern
    (lower_identifier) @variable.parameter))

(keyword_parameter
  (lower_identifier) @property)

(struct_declaration
  (upper_identifier) @type)

(enum_declaration
  (upper_identifier) @type)

(newtype_declaration
  (upper_identifier) @type)

(type_alias_declaration
  (upper_identifier) @type)

(namespace_declaration
  (type_path
    (upper_identifier) @type))

(extend_declaration
  (type_path
    (upper_identifier) @type))

(type_parameter) @type.parameter
(type_path
  (upper_identifier) @type)

(enum_variant_declaration
  (upper_identifier) @constructor)

(variant_pattern
  (upper_identifier) @constructor)

(variant_pattern
  (type_path
    (upper_identifier) @constructor))

(struct_field_declaration
  (lower_identifier) @property)

(record_field
  (lower_identifier) @property)

(struct_pattern_field
  (lower_identifier) @property)

(field_path
  (lower_identifier) @property)

(field_expression
  (lower_identifier) @property)

(field_expression
  (upper_identifier) @constructor)

(method_adapter_expression
  (lower_identifier) @property)

(method_adapter_expression
  (upper_identifier) @constructor)

(tuple_index) @property
(lower_identifier) @variable
(upper_identifier) @type
