const PREC = {
  pipe: 1,
  with: 2,
  logical_or: 3,
  logical_and: 4,
  equality: 5,
  comparison: 6,
  range: 7,
  additive: 8,
  multiplicative: 9,
  unary: 10,
  type_ops: 11,
  application: 12,
  postfix: 13,
};

module.exports = grammar({
  name: "vixen",

  word: ($) => $.lower_identifier,

  extras: ($) => [/\s/, $.doc_comment, $.line_comment, $.block_comment],

  supertypes: ($) => [
    $.expression,
    $.pattern,
    $.type_expression,
    $.statement,
  ],

  conflicts: ($) => [
    [$.tuple_expression, $.parenthesized_expression],
    [$.tuple_pattern, $.parenthesized_pattern],
    [$.tuple_type, $.parenthesized_type],
    [$.unit_pattern, $.unit_expression],
    [$.unit_expression, $.closure_parameter_clause],
    [$.array_pattern, $.array_expression],
    [$.type_path, $.identifier_expression],
    [$.binding_pattern, $.pattern],
    [$.binding_pattern, $.pattern, $.identifier_expression],
    [$.binding_pattern, $.identifier_expression],
    [$.literal_pattern, $.primary_expression],
    [$.type_path, $.variant_pattern, $.identifier_expression],
    [$.type_path, $.variant_pattern],
    [$.record_expression, $.map_expression],
    [$.block, $.map_expression, $.record_expression],
    [$.struct_pattern, $.block, $.map_expression, $.record_expression],
    [$.struct_pattern, $.map_expression, $.record_expression],
    [$.pattern, $.identifier_expression],
    [$.type_expression, $.optional_type],
    [$.wildcard_pattern, $.pipe_placeholder],
    [$.identifier_expression, $.record_field],
    [$.expression, $._postfix_target],
    [$.struct_pattern_field, $.record_field],
    [$.struct_pattern_field, $.identifier_expression, $.record_field],
  ],

  rules: {
    // Top level is the same statement model as a block body, but file-scope
    // semicolons remain optional between statements.
    document: ($) => repeat(seq($.statement, optional(";"))),

    statement_list: ($) => seq($.statement, repeat(seq(";", $.statement)), optional(";")),

    statement: ($) =>
      choice(
        $.function_declaration,
        $.struct_declaration,
        $.enum_declaration,
        $.newtype_declaration,
        $.type_alias_declaration,
        $.namespace_declaration,
        $.extend_declaration,
        $.binding_statement,
        $.compound_update_statement,
        $.field_assignment_statement,
        $.return_statement,
        $.expression,
      ),

    doc_comment: ($) => token(seq("///", /[^\n]*/)),

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
        "#[",
        commaSep1($.attribute_entry),
        optional(","),
        "]",
      ),

    attribute_entry: ($) =>
      choice(
        seq(field("key", $.lower_identifier), "=", field("value", $.expression)),
        field("key", $.lower_identifier),
      ),

    function_declaration: ($) =>
      prec.right(seq(
        optional(field("attributes", $.attribute_list)),
        "fn",
        field("name", choice($.lower_identifier, $.upper_identifier)),
        optional(field("type_parameters", $.type_parameter_list)),
        field("parameters", $.parameter_clause),
        optional(field("capabilities", $.capability_list)),
        optional(field("return_type", $.return_type)),
        optional(field("body", $.block)),
      )),

    struct_declaration: ($) =>
      prec.right(seq(
        optional(field("attributes", $.attribute_list)),
        "struct",
        field("name", $.upper_identifier),
        optional(field("type_parameters", $.type_parameter_list)),
        optional(field("body", $.struct_declaration_body)),
      )),

    enum_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "enum",
        field("name", $.upper_identifier),
        optional(field("type_parameters", $.type_parameter_list)),
        field("body", $.enum_declaration_body),
      ),

    newtype_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "newtype",
        field("name", $.upper_identifier),
        optional(field("type_parameters", $.type_parameter_list)),
        "of",
        field("value", $.type_expression),
      ),

    namespace_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "namespace",
        field("name", $.type_path),
        field("body", $.block),
      ),

    extend_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "extend",
        field("name", $.type_path),
        field("body", $.block),
      ),

    type_alias_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "type",
        field("name", $.upper_identifier),
        optional(field("type_parameters", $.type_parameter_list)),
        "=",
        field("value", $.type_alias_value),
      ),

    type_alias_value: ($) => choice($.sum_type_definition, $.type_expression),

    struct_declaration_body: ($) =>
      seq("{", optional(commaSep1($.struct_field_declaration)), optional(","), "}"),

    struct_field_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        field("name", $.lower_identifier),
        ":",
        field("type", $.slot_type),
        optional(seq("=", field("value", $.expression))),
      ),

    slot_type: ($) => choice($.inline_type_declaration, $.type_expression),

    inline_type_declaration: ($) =>
      choice(
        $.inline_struct_declaration,
        $.inline_enum_declaration,
        $.inline_newtype_declaration,
        $.inline_type_alias_declaration,
      ),

    inline_struct_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "struct",
        optional(field("name", $.upper_identifier)),
        optional(field("body", $.struct_declaration_body)),
      ),

    inline_enum_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "enum",
        optional(field("name", $.upper_identifier)),
        field("body", $.enum_declaration_body),
      ),

    inline_newtype_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "newtype",
        optional(field("name", $.upper_identifier)),
        "of",
        field("value", $.type_expression),
      ),

    inline_type_alias_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "type",
        optional(field("name", $.upper_identifier)),
        "=",
        field("value", $.type_alias_value),
      ),

    enum_declaration_body: ($) =>
      seq("{", optional(commaSep1($.enum_variant_declaration)), optional(","), "}"),

    enum_variant_declaration: ($) =>
      seq(
        optional(field("attributes", $.attribute_list)),
        "struct",
        field("name", $.upper_identifier),
        optional(field("body", $.struct_declaration_body)),
      ),

    sum_type_definition: ($) => prec.left(seq($.type_expression, repeat1(seq("|", $.type_expression)))),

    binding_statement: ($) =>
      seq(
        "let",
        field("pattern", $.binding_pattern),
        optional(seq(":", field("type", $.slot_type))),
        "=",
        field("value", $.expression),
      ),

    compound_update_statement: ($) =>
      seq(field("name", $.lower_identifier), "with=", field("value", $.update_body)),

    field_assignment_statement: ($) =>
      seq(field("target", $.field_assignment_target), "=", field("value", $.expression)),

    field_assignment_target: ($) => $.field_expression,

    return_statement: ($) => prec.right(seq("return", optional($.expression))),

    parameter_clause: ($) =>
      seq(
        "(",
        optional($.parameter_list),
        optional($.keyword_parameter_section),
        ")",
      ),

    parameter_list: ($) => seq(commaSep1($.parameter), optional(",")),

    keyword_parameter_section: ($) =>
      seq(";", optional(seq(commaSep1($.keyword_parameter), optional(",")))),

    parameter: ($) => seq(field("pattern", $.binding_pattern), ":", field("type", $.slot_type)),

    keyword_parameter: ($) =>
      seq(
        field("name", $.lower_identifier),
        ":",
        field("type", $.slot_type),
        optional(seq("=", field("default", $.expression))),
      ),

    return_type: ($) => seq("->", $.type_expression),

    type_parameter_list: ($) =>
      seq("[", optional(commaSep1($.type_parameter)), optional(","), "]"),

    type_parameter: ($) => $.upper_identifier,

    type_expression: ($) => choice($.optional_type, $.type_primary),

    type_primary: ($) =>
      choice(
        $.function_type,
        $.unit_type,
        $.tuple_type,
        $.parenthesized_type,
        $.type_path,
      ),

    optional_type: ($) => prec.right(seq($.type_primary, "?")),

    parenthesized_type: ($) => seq("(", $.type_expression, ")"),

    unit_type: ($) => seq("(", ")"),

    tuple_type: ($) => seq("(", commaSep1($.type_expression), optional(","), ")"),

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
      prec.right(
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
        $.unit_pattern,
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
        $.unit_pattern,
        $.tuple_pattern,
        $.struct_pattern,
        $.array_pattern,
        $.variant_pattern,
        $.lower_identifier,
        $.parenthesized_pattern,
      ),

    parenthesized_pattern: ($) => seq("(", $.pattern, ")"),

    wildcard_pattern: ($) => "_",

    unit_pattern: ($) => seq("(", ")"),

    literal_pattern: ($) =>
      choice(
        $.integer_literal,
        $.float_literal,
        $.boolean_literal,
        $.string_literal,
      ),

    or_pattern: ($) => prec.left(seq($.pattern, "|", $.pattern)),

    at_pattern: ($) =>
      prec.right(seq(field("name", $.lower_identifier), "@", field("pattern", $.pattern))),

    tuple_pattern: ($) => seq("(", commaSep1($.pattern), optional(","), ")"),

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

    array_pattern_item: ($) => choice($.pattern, $.array_rest_pattern),

    array_rest_pattern: ($) => prec(20, choice("..", seq("..", $.lower_identifier))),

    variant_pattern: ($) =>
      prec.right(
        seq(
          field("name", choice($.type_path, $.upper_identifier)),
          optional($.struct_pattern),
        ),
      ),

    expression: ($) =>
      choice(
        $.if_expression,
        $.match_expression,
        $.arrow_closure,
        $.pipe_expression,
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
        $.keyword_application_expression,
        $.application_expression,
        $.primary_expression,
        $.field_expression,
        $.index_expression,
        $.invocation_expression,
        $.fallible_expression,
      ),

    primary_expression: ($) =>
      choice(
        $.identifier_expression,
        $.integer_literal,
        $.float_literal,
        $.boolean_literal,
        $.string_literal,
        $.interpolated_string,
        $.path_literal,
        $.interpolated_path_literal,
        $.array_expression,
        $.record_expression,
        $.map_expression,
        $.unit_expression,
        $.tuple_expression,
        $.parenthesized_expression,
        $.block,
        $.import_expression,
        $.fail_expression,
        $.method_adapter_expression,
        $.pipe_placeholder,
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

    path_literal: ($) => token(seq("@", /[A-Za-z0-9_./-]+/)),

    interpolated_path_literal: ($) =>
      seq(
        "@(",
        repeat(
          choice(
            alias(token.immediate(/[A-Za-z0-9_./-]+/), $.path_content),
            $.path_interpolation,
          ),
        ),
        ")",
      ),

    path_interpolation: ($) => seq("{", $.expression, "}"),

    escape_sequence: ($) => token(/\\([\\`"nrt0]|u\{[0-9A-Fa-f]{1,6}\})/),

    unit_expression: ($) => seq("(", ")"),

    tuple_expression: ($) =>
      seq("(", commaSep1(choice($.expression, $.spread_expression)), optional(","), ")"),

    parenthesized_expression: ($) => seq("(", $.expression, ")"),

    block: ($) => seq("{", optional($.statement_list), "}"),

    array_expression: ($) =>
      seq("[", optional(commaSep1(choice($.expression, $.spread_expression))), optional(","), "]"),

    map_expression: ($) =>
      seq("{", optional(commaSep1($.map_entry)), optional(","), "}"),

    map_entry: ($) => seq(field("key", $.map_key_expression), ":", field("value", $.expression)),

    map_key_expression: ($) =>
      choice(
        $.integer_literal,
        $.float_literal,
        $.boolean_literal,
        $.string_literal,
        $.interpolated_string,
        $.path_literal,
        $.interpolated_path_literal,
        $.parenthesized_expression,
        $.tuple_expression,
        $.array_expression,
        $.record_expression,
        $.map_expression,
        $.method_adapter_expression,
        $.field_expression,
        $.index_expression,
        $.invocation_expression,
        $.fallible_expression,
        $.upper_identifier,
        $.dollar_identifier,
      ),

    record_expression: ($) =>
      seq("{", optional(commaSep1($.record_field)), optional(","), "}"),

    record_field: ($) =>
      choice(
        seq(field("name", $.lower_identifier), ":", field("value", $.expression)),
        field("shorthand", $.lower_identifier),
        seq("..", field("spread", $.expression)),
      ),

    spread_expression: ($) => prec.right(20, seq("..", $.expression)),

    update_body: ($) => seq("{", optional(commaSep1($.update_field)), optional(","), "}"),

    update_field: ($) =>
      choice(
        seq(field("path", $.field_path), ":", field("value", $.expression)),
        seq(":", field("shorthand", $.field_path), "with", field("value", $.update_body)),
        seq("..", field("spread", $.expression)),
      ),

    field_path: ($) =>
      seq($.lower_identifier, repeat(seq(".", $.lower_identifier))),

    application_expression: ($) =>
      choice(
        prec.left(
          PREC.application + 1,
          seq(
            field(
              "function",
              choice(
                $._postfix_target,
                $.application_expression,
                $.keyword_application_expression,
              ),
            ),
            field("argument", $.map_expression),
          ),
        ),
        prec.left(
          PREC.application + 1,
          seq(
            field(
              "function",
              choice(
                $._postfix_target,
                $.application_expression,
                $.keyword_application_expression,
              ),
            ),
            field("argument", $.record_expression),
          ),
        ),
        prec.left(
          PREC.application,
          seq(
            field(
              "function",
              choice(
                $._postfix_target,
                $.application_expression,
                $.keyword_application_expression,
              ),
            ),
            field("argument", $._application_argument),
          ),
        ),
      ),

    _application_argument: ($) =>
      choice(
        $._postfix_target,
        $.if_expression,
        $.match_expression,
        $.arrow_closure,
      ),

    keyword_application_expression: ($) =>
      choice(
        prec.left(
          PREC.application,
          seq(
            field(
              "function",
              choice(
                $._postfix_target,
                $.application_expression,
                $.keyword_application_expression,
              ),
            ),
            "where",
            field("argument", $.record_expression),
          ),
        ),
        prec.left(
          PREC.application,
          seq(
            field(
              "function",
              choice(
                $._postfix_target,
                $.application_expression,
                $.keyword_application_expression,
              ),
            ),
            "where",
            field("argument", $.expression),
          ),
        ),
      ),

    index_expression: ($) =>
      prec.left(
        PREC.postfix,
        seq(field("value", $._postfix_target), $._immediate_l_bracket, field("index", $.expression), "]"),
      ),

    field_expression: ($) =>
      prec.left(
        PREC.postfix,
        seq(
          field("value", $._postfix_target),
          $._immediate_dot,
          field("field", choice($.lower_identifier, $.upper_identifier, $.tuple_index)),
        ),
      ),

    invocation_expression: ($) =>
      prec.left(PREC.postfix, seq(field("value", $._postfix_target), $._immediate_bang)),

    fallible_expression: ($) =>
      prec.left(PREC.postfix, seq(field("value", $._postfix_target), $._immediate_question)),

    _postfix_target: ($) =>
      choice(
        $.primary_expression,
        $.field_expression,
        $.index_expression,
        $.invocation_expression,
        $.fallible_expression,
      ),

    tuple_index: ($) => /_[0-9]+/,

    if_expression: ($) =>
      prec.right(
        seq(
          "if",
          field("condition", $.expression),
          field("consequence", $.block),
          field("alternative", $.else_clause),
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

    pipe_expression: ($) =>
      prec.left(PREC.pipe, seq(field("value", $.expression), "|>", field("function", $.expression))),

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

    closure_parameter_clause: ($) =>
      seq("(", optional(commaSep1($.binding_pattern)), optional(","), ")"),

    arrow_closure: ($) =>
      prec.right(seq(field("parameters", $.closure_parameter_clause), "=>", field("body", $.expression))),

    dollar_identifier: ($) => /\$[0-9]*/,

    pipe_placeholder: ($) => "_",

    method_adapter_expression: ($) =>
      prec.right(
        seq(".", field("field", choice($.lower_identifier, $.upper_identifier, $.tuple_index)), optional($._immediate_bang)),
      ),

    _immediate_l_bracket: ($) => token.immediate("["),

    _immediate_dot: ($) => token.immediate("."),

    _immediate_bang: ($) => token.immediate("!"),

    _immediate_question: ($) => token.immediate("?"),

    lower_identifier: ($) => /(?:[a-z][A-Za-z0-9_]*|_[A-Za-z0-9_]+)/,

    upper_identifier: ($) => /[A-Z][A-Za-z0-9_]*/,
  },
});

function commaSep1(rule) {
  return seq(rule, repeat(seq(",", rule)));
}
