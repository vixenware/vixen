[
  "fn"
  "struct"
  "enum"
  "newtype"
  "of"
  "with"
  "match"
  "if"
  "else"
  "is"
  "as"
  "as?"
  "import"
  "return"
  "fail"
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
  "@"
  "#"
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
  ".."
  "..="
] @operator

(line_comment) @comment
(block_comment) @comment
(escape_sequence) @string.escape
(string_literal) @string
(interpolated_string) @string.special
(string_content) @string
(integer_literal) @number
(float_literal) @number.float
(dollar_identifier) @variable.special

(attribute
  name: (lower_identifier) @attribute)

(capability_tag
  "#" @operator
  (lower_identifier) @label)

(function_declaration
  (lower_identifier) @function)

(parameter
  (binding_pattern
    (lower_identifier) @variable.parameter))

(keyword_parameter
  (lower_identifier) @property)

(type_declaration
  (upper_identifier) @type)

(type_parameter) @type.parameter
(type_path
  (upper_identifier) @type)

(enum_variant
  (upper_identifier) @constructor)

(variant_pattern
  (upper_identifier) @constructor)

(variant_pattern
  (type_path
    (upper_identifier) @constructor))

(struct_field_declaration
  (lower_identifier) @property)

(struct_literal_field
  (lower_identifier) @property)

(struct_pattern_field
  (lower_identifier) @property)

(field_path
  (lower_identifier) @property)

(keyword_argument
  (lower_identifier) @property)

(field_expression
  (lower_identifier) @property)

(field_expression
  (upper_identifier) @constructor)

(tuple_index) @property
(lower_identifier) @variable
(upper_identifier) @type
