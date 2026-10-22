; From tree-sitter-proto 0.2.0 (MIT, github.com/coder3101/tree-sitter-proto), which
; ships it without exporting it. Added: the keywords and syntax version it left out,
; and RPC names shown as functions.

[
  "syntax"
  "edition"
  "package"
  "option"
  "import"
  "service"
  "rpc"
  "returns"
  "message"
  "enum"
  "oneof"
  "repeated"
  "reserved"
  "to"
  "optional"
  "stream"
  "map"
  "extend"
  "extensions"
  "weak"
  "public"
  "max"
] @keyword

(rpc_name) @function

[
  (key_type)
  (type)
  (message_name)
  (enum_name)
  (service_name)
]@type

[
  (string)
  "\"proto3\""
  "\"proto2\""
] @string

[
  (int_lit)
  (float_lit)
] @number

[
  (true)
  (false)
] @constant.builtin

(comment) @comment

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
]  @punctuation.bracket

