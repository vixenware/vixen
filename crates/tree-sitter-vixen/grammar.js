const PREC = {
  with: 1,
  logical_or: 2,
  logical_and: 3,
  equality: 4,
  comparison: 5,
  range: 6,
  additive: 7,
  multiplicative: 8,
  unary: 9,
  type_ops: 10,
  postfix_fallible: 11,
  field: 12,
  call: 13,
};

module.exports = grammar({
  name: "vixen",

  word: ($) => $.lower_identifier,

  extras: ($) => [/\s/, $.line_comment, $.block_comment],

  supertypes: ($) => [$.expression, $.pattern, $.type_expression, $.statement],

  conflicts: ($) => [
    [$.tuple_expression, $.parenthesized_expression],
    [$.tuple_pattern, $.parenthesized_pattern],
    [$.tuple_type, $.parenthesized_type],
    [$.array_pattern, $.array_expression],
    [$.type_path, $.identifier_expression],
    [$.attribute, $.identifier_expression],
    [$.binding_pattern, $.pattern],
    [$.binding_pattern, $.pattern, $.identifier_expression],
    [$.binding_pattern, $.identifier_expression],
    [$.literal_pattern, $.primary_expression],
    [$.type_path, $.variant_pattern, $.identifier_expression],
    [$.type_path, $.variant_pattern],
    [$.type_expression, $.variant_pattern],
    [$.struct_pattern_field, $.identifier_expression],
    [$.struct_pattern, $.block],
    [$.struct_pattern, $.struct_literal_body],
    [$.binding_pattern, $.struct_pattern_field],
    [$.pattern, $.identifier_expression],
  ],

  rules: {
    document: ($) => optional($.statement_list),

    statement_list: ($) => seq($.statement, repeat(seq(";", $.statement)), optional(";")),

    statement: ($) =>
      choice(
        $.function_declaration,
        $.type_declaration,
        $.binding_statement,
        $.compound_update_statement,
        $.field_assignment_statement,
        $.return_statement,
        $.expression,
      ),

    line_comment: ($) => token(seq("//", /[^\n]*/)),

    block_comment: ($) =>
      seq(
        "/*",
        repeat(
          choice(
            /[^/*]+/,
            /\/[^*]/,
            /\*[^/]/,
            $.block_comment,
          ),
        ),
        "*/",
      ),

    attribute_list: ($) => repeat1($.attribute),

    attribute: ($) =>
      seq(
        "@",
        field("name", $.lower_identifier),
        optional(field("arguments", $.attribute_arguments)),
      ),

    attribute_arguments: ($) =>
      seq("(", optional(commaSep1($.expression)), optional(","), ")"),

    function_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "fn",
        field("name", $.lower_identifier),
        optional(field("type_parameters", $.type_parameter_list)),
        field("parameters", $.parameter_clause),
        optional(field("return_type", $.return_type)),
        optional(field("body", $.block)),
      ),

    type_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        field("name", $.upper_identifier),
        "=",
        field(
          "value",
          choice(
            $.struct_type_definition,
            $.enum_type_definition,
            $.newtype_type_definition,
            $.sum_type_definition,
          ),
        ),
      ),

    struct_type_definition: ($) => seq("struct", field("body", $.struct_type_body)),

    struct_type_body: ($) =>
      seq("{", optional(commaSep1($.struct_field_declaration)), optional(","), "}"),

    struct_field_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        field("name", $.lower_identifier),
        ":",
        field("type", $.type_expression),
        optional(
          seq(
            "=",
            field(
              "value",
              choice(
                $.struct_type_definition,
                $.enum_type_definition,
                $.newtype_type_definition,
                $.expression,
              ),
            ),
          ),
        ),
      ),

    enum_type_definition: ($) => seq("enum", field("body", $.enum_type_body)),

    enum_type_body: ($) =>
      seq("{", optional(commaSep1($.enum_variant)), optional(","), "}"),

    enum_variant: ($) =>
      choice(
        seq(
          optional(field("attributes", $.attribute_list)),
          field("name", $.upper_identifier),
          optional(
            choice(
              field("tuple_payload", $.tuple_variant_payload),
              field("struct_payload", $.struct_variant_payload),
              field("alias_payload", $.alias_variant_payload),
            ),
          ),
        ),
        seq(
          optional(field("attributes", $.attribute_list)),
          ":",
          field("alias_type", $.type_expression),
        ),
      ),

    tuple_variant_payload: ($) =>
      seq("(", optional(commaSep1($.type_expression)), optional(","), ")"),

    struct_variant_payload: ($) =>
      seq("{", optional(commaSep1($.struct_field_declaration)), optional(","), "}"),

    alias_variant_payload: ($) => seq(":", $.type_expression),

    newtype_type_definition: ($) => seq("newtype", "of", $.type_expression),

    sum_type_definition: ($) => prec.left(seq($.type_expression, repeat1(seq("|", $.type_expression)))),

    binding_statement: ($) =>
      seq(
        field("pattern", $.binding_pattern),
        optional(seq(":", field("type", $.type_expression))),
        "=",
        field("value", $.expression),
      ),

    compound_update_statement: ($) =>
      seq(field("name", $.lower_identifier), "with=", field("value", $.update_body)),

    field_assignment_statement: ($) =>
      seq(field("target", $.field_assignment_target), "=", field("value", $.expression)),

    field_assignment_target: ($) => $.field_expression,

    return_statement: ($) => seq("return", optional($.expression)),

    parameter_clause: ($) => seq("(", optional(commaSep1($.parameter_item)), optional(","), ")"),

    parameter_item: ($) => choice($.parameter, $.keyword_parameter_group),

    parameter: ($) => seq(field("pattern", $.binding_pattern), ":", field("type", $.type_expression)),

    keyword_parameter_group: ($) =>
      seq("{", optional(commaSep1($.keyword_parameter)), optional(","), "}"),

    keyword_parameter: ($) =>
      seq(
        field("name", $.lower_identifier),
        ":",
        field("type", $.type_expression),
        optional(seq("=", field("default", $.expression))),
      ),

    return_type: ($) => seq("->", $.type_expression),

    type_parameter_list: ($) =>
      seq("[", optional(commaSep1($.type_parameter)), optional(","), "]"),

    type_parameter: ($) => $.upper_identifier,

    type_expression: ($) =>
      choice(
        $.function_type,
        $.tuple_type,
        $.parenthesized_type,
        $.type_path,
      ),

    parenthesized_type: ($) => seq("(", $.type_expression, ")"),

    tuple_type: ($) =>
      seq("(", commaSep1($.type_expression), optional(","), ")"),

    function_type: ($) =>
      seq(
        "Fn",
        "(",
        optional(commaSep1($.type_expression)),
        optional(","),
        ")",
        optional($.capability_list),
        "->",
        $.type_expression,
      ),

    capability_list: ($) => repeat1($.capability_tag),

    capability_tag: ($) => seq("#", $.lower_identifier),

    type_path: ($) =>
      prec.left(
        seq(
          field("head", $.upper_identifier),
          repeat(seq(".", field("member", $.upper_identifier))),
          optional(field("type_arguments", $.type_arguments)),
        ),
      ),

    type_arguments: ($) => seq("[", optional(commaSep1($.type_expression)), optional(","), "]"),

    binding_pattern: ($) =>
      choice(
        $.wildcard_pattern,
        $.lower_identifier,
        $.tuple_pattern,
        $.struct_pattern,
        $.array_pattern,
        $.parenthesized_pattern,
      ),

    pattern: ($) =>
      choice(
        $.or_pattern,
        $.at_pattern,
        $.wildcard_pattern,
        $.literal_pattern,
        $.tuple_pattern,
        $.struct_pattern,
        $.array_pattern,
        $.variant_pattern,
        $.lower_identifier,
        $.parenthesized_pattern,
      ),

    parenthesized_pattern: ($) => seq("(", $.pattern, ")"),

    wildcard_pattern: ($) => "_",

    literal_pattern: ($) =>
      choice(
        $.integer_literal,
        $.float_literal,
        $.boolean_literal,
        $.string_literal,
      ),

    or_pattern: ($) => prec.left(seq($.pattern, "|", $.pattern)),

    at_pattern: ($) => prec.right(seq(field("name", $.lower_identifier), "@", field("pattern", $.pattern))),

    tuple_pattern: ($) =>
      seq("(", commaSep1($.pattern), optional(","), ")"),

    struct_pattern: ($) =>
      seq("{", optional(commaSep1($.struct_pattern_field)), optional(","), "}"),

    struct_pattern_field: ($) =>
      choice(
        seq(field("name", $.lower_identifier), ":", field("pattern", $.pattern)),
        field("name", $.lower_identifier),
        prec(20, ".."),
      ),

    array_pattern: ($) =>
      seq("[", optional(commaSep1($.array_pattern_item)), optional(","), "]"),

    array_pattern_item: ($) =>
      choice($.pattern, $.array_rest_pattern),

    array_rest_pattern: ($) => prec(20, choice("..", seq("..", $.lower_identifier))),

    variant_pattern: ($) =>
      prec.right(
        seq(
          field("name", choice($.type_path, $.upper_identifier)),
          optional(choice($.tuple_pattern, $.struct_pattern)),
        ),
      ),

    expression: ($) =>
      choice(
        $.if_expression,
        $.match_expression,
        $.arrow_closure,
        $.closure_expression,
        $.with_expression,
        $.logical_or_expression,
        $.logical_and_expression,
        $.equality_expression,
        $.comparison_expression,
        $.range_expression,
        $.additive_expression,
        $.multiplicative_expression,
        $.unary_expression,
        $.cast_expression,
        $.type_ascription_expression,
        $.is_expression,
        $.fallible_expression,
        $.field_expression,
        $.call_expression,
        $.index_expression,
        $.partial_application_expression,
        $.struct_literal_expression,
        $.primary_expression,
      ),

    primary_expression: ($) =>
      choice(
        $.identifier_expression,
        $.integer_literal,
        $.float_literal,
        $.boolean_literal,
        $.string_literal,
        $.interpolated_string,
        $.array_expression,
        $.map_expression,
        $.tuple_expression,
        $.parenthesized_expression,
        $.block,
        $.import_expression,
        $.fail_expression,
        $.dollar_identifier,
      ),

    identifier_expression: ($) => choice($.lower_identifier, $.upper_identifier),

    integer_literal: ($) => /\d(?:[\d_]*\d)?/,

    float_literal: ($) => /\d(?:[\d_]*\d)?\.\d(?:[\d_]*\d)?/,

    boolean_literal: ($) => choice("true", "false"),

    string_literal: ($) =>
      seq(
        "\"",
        repeat(choice($.escape_sequence, alias(token.immediate(/[^"\\\n]+/), $.string_content))),
        "\"",
      ),

    interpolated_string: ($) =>
      seq(
        "`",
        repeat(
          choice(
            $.escape_sequence,
            alias(token.immediate(/[^`\\$]+/), $.string_content),
            alias(token.immediate(/\$/), $.string_content),
            $.string_interpolation,
          ),
        ),
        "`",
      ),

    string_interpolation: ($) => seq("${", $.expression, "}"),

    escape_sequence: ($) => token(/\\([\\`"nrt0]|u\{[0-9A-Fa-f]{1,6}\})/),

    tuple_expression: ($) =>
      seq("(", commaSep1(choice($.expression, $.spread_expression)), optional(","), ")"),

    parenthesized_expression: ($) => seq("(", $.expression, ")"),

    block: ($) => seq("{", optional($.statement_list), "}"),

    array_expression: ($) =>
      seq("[", optional(commaSep1(choice($.expression, $.spread_expression))), optional(","), "]"),

    map_expression: ($) =>
      seq("%{", optional(commaSep1($.map_entry)), optional(","), "}"),

    map_entry: ($) => seq(field("key", $.expression), "=>", field("value", $.expression)),

    spread_expression: ($) => prec.right(20, seq("..", $.expression)),

    struct_literal_expression: ($) =>
      seq(field("type", $.type_path), field("body", $.struct_literal_body)),

    struct_literal_body: ($) =>
      seq("{", optional(commaSep1($.struct_literal_field)), optional(","), "}"),

    struct_literal_field: ($) =>
      choice(
        seq(field("name", $.lower_identifier), ":", field("value", $.expression)),
        seq(":", field("shorthand", $.lower_identifier)),
        seq("..", field("spread", $.expression)),
      ),

    update_body: ($) => seq("{", optional(commaSep1($.update_field)), optional(","), "}"),

    update_field: ($) =>
      choice(
        seq(field("path", $.field_path), ":", field("value", $.expression)),
        seq(":", field("shorthand", $.field_path), "with", field("value", $.update_body)),
        seq("..", field("spread", $.expression)),
      ),

    field_path: ($) =>
      seq($.lower_identifier, repeat(seq(".", $.lower_identifier))),

    call_expression: ($) =>
      prec.left(PREC.call, seq(field("function", $.expression), field("arguments", $.argument_list))),

    argument_list: ($) => seq("(", optional(commaSep1($.argument)), optional(","), ")"),

    argument: ($) =>
      choice(
        $.keyword_argument,
        $.argument_placeholder,
        $.spread_expression,
        $.expression,
      ),

    keyword_argument: ($) =>
      seq(field("name", $.lower_identifier), ":", field("value", $.expression)),

    argument_placeholder: ($) => prec(20, ".."),

    index_expression: ($) =>
      prec.left(PREC.call, seq(field("value", $.expression), "[", field("index", $.expression), "]")),

    field_expression: ($) =>
      prec.left(
        PREC.field,
        seq(
          field("value", $.expression),
          ".",
          field("field", choice($.lower_identifier, $.upper_identifier, $.tuple_index)),
        ),
      ),

    tuple_index: ($) => /_[0-9]+/,

    partial_application_expression: ($) =>
      prec.right(seq("@", field("function", $.expression), field("arguments", $.argument_list))),

    if_expression: ($) =>
      prec.right(
        seq(
          "if",
          field("condition", $.expression),
          field("consequence", $.block),
          optional(field("alternative", $.else_clause)),
        ),
      ),

    else_clause: ($) => seq("else", choice($.if_expression, $.block)),

    match_expression: ($) =>
      seq("match", field("value", $.expression), "{", optional(commaSep1($.match_arm)), optional(","), "}"),

    match_arm: ($) =>
      seq(
        field("pattern", $.pattern),
        optional(seq("if", field("guard", $.expression))),
        "=>",
        field("value", $.expression),
      ),

    import_expression: ($) => seq("import", "(", field("path", $.string_literal), ")"),

    fail_expression: ($) => prec.right(PREC.unary, seq("fail", $.expression)),

    unary_expression: ($) =>
      prec.right(PREC.unary, seq(choice("-", "!"), field("argument", $.expression))),

    logical_or_expression: ($) =>
      prec.left(PREC.logical_or, seq($.expression, "||", $.expression)),

    logical_and_expression: ($) =>
      prec.left(PREC.logical_and, seq($.expression, "&&", $.expression)),

    equality_expression: ($) =>
      prec.left(PREC.equality, seq($.expression, choice("==", "!="), $.expression)),

    comparison_expression: ($) =>
      prec.left(PREC.comparison, seq($.expression, choice("<", "<=", ">", ">="), $.expression)),

    range_expression: ($) =>
      choice(
        prec.left(PREC.range, seq($.expression, choice("..", "..="), $.expression)),
        prec.left(PREC.range, seq($.expression, "..")),
        prec.right(PREC.range, seq(choice("..", "..="), $.expression)),
        "..",
      ),

    additive_expression: ($) =>
      prec.left(PREC.additive, seq($.expression, choice("+", "-"), $.expression)),

    multiplicative_expression: ($) =>
      prec.left(PREC.multiplicative, seq($.expression, choice("*", "/", "%"), $.expression)),

    cast_expression: ($) =>
      prec.left(PREC.type_ops, seq($.expression, choice("as", "as?"), $.type_expression)),

    type_ascription_expression: ($) =>
      prec.left(PREC.type_ops, seq($.expression, "::", $.type_expression)),

    is_expression: ($) =>
      prec.left(PREC.type_ops, seq($.expression, "is", $.pattern)),

    with_expression: ($) =>
      prec.right(PREC.with, seq(field("value", $.expression), "with", field("changes", $.update_body))),

    closure_expression: ($) =>
      seq("fn", field("parameters", $.closure_parameter_clause), field("body", $.block)),

    closure_parameter_clause: ($) =>
      seq("(", optional(commaSep1($.binding_pattern)), optional(","), ")"),

    arrow_closure: ($) =>
      prec.right(seq(field("parameters", $.closure_parameter_clause), "=>", field("body", $.expression))),

    fallible_expression: ($) => prec.left(PREC.postfix_fallible, seq($.expression, "?")),

    dollar_identifier: ($) => /\$[0-9]*/,

    lower_identifier: ($) => /[a-z_][A-Za-z0-9_]*/,

    upper_identifier: ($) => /[A-Z][A-Za-z0-9_]*/,
  },
});

function commaSep1(rule) {
  return seq(rule, repeat(seq(",", rule)));
}
