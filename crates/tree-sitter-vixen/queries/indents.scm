(block "}" @end) @indent
(struct_declaration_body "}" @end) @indent
(enum_declaration_body "}" @end) @indent
(record_expression "}" @end) @indent
(map_expression "}" @end) @indent
(update_body "}" @end) @indent
(match_expression "}" @end) @indent
(array_expression "]" @end) @indent
(tuple_expression ")" @end) @indent

; While typing, unfinished open delimiters recover as ERROR nodes instead of
; their final syntax nodes. Keep indent active to EOF from the delimiter.
(document
  (ERROR "{" @start)
  (#eq? @start "{")
) @indent

(document
  (ERROR "[" @start)
  (#eq? @start "[")
) @indent

(document
  (ERROR "(" @start)
  (#eq? @start "(")
) @indent
