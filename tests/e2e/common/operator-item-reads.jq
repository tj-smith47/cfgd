# Reads the syntax tree `shfmt --to-json` prints for one script and prints the
# line of each command that picks one operator pod, per the contract above
# scan_operator_item_reads in test-pr-install.sh. A unit is one pipeline (a
# lone command is a pipeline of one stage); a $(...) inside it is a unit of its
# own, so a command substitution nested to any depth is judged where it runs.

# The text of a word with its quotes removed. A command substitution is opaque
# here because its commands are judged as units of their own.
def text:
  if type != "object" then ""
  elif .Type == "Lit" or .Type == "SglQuoted" then .Value
  elif .Type == "DblQuoted" then [.Parts[]? | text] | join("")
  elif .Type == "ParamExp" then "$" + (.Param.Value // "")
  elif .Type == "CmdSubst" then "$(...)"
  elif .Type == "ProcSubst" then "<(...)"
  elif has("Parts") then [.Parts[]? | text] | join("")
  else "" end;

# A heredoc body is data the command receives on stdin, so its text is skipped.
def redir_words: [.Redirs[]? | .Word | text];

def stmt_words:
  (if .Cmd.Type == "CallExpr" then [.Cmd.Assigns[]? | .Value | text] + [.Cmd.Args[]? | text] else [] end)
  + redir_words;

# Every statement a stage runs itself, the stage first: a brace group, a
# subshell, a loop or a conditional contributes the statements inside it. A
# $(...) or <(...) stays out because its commands are units of their own, and
# a heredoc body stays out because it is data.
def own_stmts:
  if type == "object" then
    if .Type == "CmdSubst" or .Type == "ProcSubst" then empty
    else (select(has("Cmd")), (to_entries[] | select(.key != "Hdoc") | .value | own_stmts)) end
  elif type == "array" then .[] | own_stmts
  else empty end;

# One word list per command the stage runs, with where that command starts.
def stage_calls: [own_stmts | {offset: .Pos.Offset, words: stmt_words}];

def stages:
  if .Cmd.Type == "BinaryCmd" and (.Cmd.Op == "|" or .Cmd.Op == "|&")
  then (.Cmd.X | stages), (.Cmd.Y | stages)
  else . end;

def is_pipeline:
  type == "object" and (.Cmd | type) == "object" and
  (.Cmd.Type == "CallExpr" or (.Cmd.Type == "BinaryCmd" and (.Cmd.Op == "|" or .Cmd.Op == "|&")));

# Every pipeline in document order, each stage's children searched for the
# pipelines nested inside it.
def units:
  if is_pipeline then
    [stages] as $s
    | {offset: .Pos.Offset, end: .End.Offset, line: .Pos.Line, stages: $s},
      ($s[] | (.Cmd, .Redirs) | .[]? | units)
  elif type == "object" or type == "array" then .[] | units
  else empty end;

def picks($sorted):
  test("\\.items\\[-?[0-9]+\\]") or
  (test("\\.items\\[-?[0-9]*:-?[0-9]*\\]") and ($sorted | not)) or
  test("\\.items(\\[\\])?(\\s*\\|\\s*sort_by\\([^)]*\\))?\\s*\\|\\s*first([^A-Za-z0-9_]|$)") or
  test("\\[\\s*\\.items\\[\\][^\\]]*\\]\\s*\\|\\s*first([^A-Za-z0-9_]|$)") or
  test("(^|[^A-Za-z0-9_])first\\(\\s*\\.items");

def kubectl_word: test("kubectl|KUBECTL");

# True when the words run kubectl get pods by an operator selector.
def operator_get:
  . as $w
  | any(.[]; kubectl_word) and
    any(range(0; length - 1); $w[.] == "get" and ($w[. + 1] | IN("pod", "pods", "po"))) and
    any(.[]; test("E2E_OPERATOR_PODS|app=cfgd-operator|component=operator"));

def json_output: join(" ") | test("(-o|--output)[\\s=]*json([^A-Za-z0-9_]|$)");

# The kubectl call a lone-stage statement makes when it keeps the operator
# pods as JSON, with whether it sorted them.
def capture_call:
  select(is_pipeline and .Cmd.Type == "CallExpr")
  | stmt_words as $w
  | select(($w | operator_get) and ($w | json_output))
  | {sorted: ($w | any(.[]; test("--sort-by")))};

# The first simple command a $(...) runs: the left of an && or || list, and
# the first statement of a brace group or subshell. A capture like
# NAME=$(kubectl ... -o json || echo "") or NAME=$( (kubectl ... -o json) )
# keeps that command's JSON when it succeeds. A pipe stops the walk: what
# reaches NAME is then the last stage's output, so the pod list is gone.
def first_command:
  if .Cmd.Type == "BinaryCmd" and (.Cmd.Op == "&&" or .Cmd.Op == "||") then .Cmd.X | first_command
  elif .Cmd.Type == "Block" or .Cmd.Type == "Subshell" then .Cmd.Stmts[0] // {} | first_command
  else . end;

def captures:
  [ ..
    | objects
    | (select(has("Name") and has("Value") and (.Value | type) == "object")
       | {kind: "var", name: .Name.Value, offset: .Pos.Offset} as $cap
       | (.Value.Parts | if length == 1 and .[0].Type == "DblQuoted" then .[0].Parts else . end)
       | select(length == 1 and .[0].Type == "CmdSubst" and (.[0].Stmts | length) >= 1)
       | .[0].Stmts[0] | first_command | capture_call
       | $cap + .),
      (select(is_pipeline and .Cmd.Type == "CallExpr")
       | [.Redirs[]? | select((.Op | IN(">", ">>", ">|", "&>", "&>>")) and ((.N.Value // "1") == "1")) | .Word | text] as $paths
       | select(($paths | length) > 0)
       | {kind: "file", name: $paths[0], offset: .Pos.Offset} as $cap
       | capture_call
       | $cap + .)
  ];

# The capture a unit reads with the latest one recorded before it, if any.
def read_capture($caps; $unit; $words):
  [ $caps[]
    | select(.offset < $unit.offset)
    | .name as $name
    | select(
        (.kind == "var" and any($unit.stages[] | .. | objects; .Type == "ParamExp" and .Param.Value == $name)) or
        (.kind == "file" and any($words[]; contains($name))))
  ] | last;

captures as $caps
| [ units
    | . as $unit
    | [.stages[] | stage_calls[]] as $calls
    | [$calls[].words[]] as $words
    | ($words | join(" ")) as $text
    | [$calls[].words | select(operator_get)] as $gets
    | if ($gets | length) > 0 then
        ($gets | any(.[][]; test("--sort-by"))) as $sorted
        | select($text | picks($sorted))
        | $unit + {calls: $calls, sorted: $sorted}
      elif ($text | kubectl_word) then empty
      else
        read_capture($caps; $unit; $words) as $cap
        | select($cap != null and ($text | picks($cap.sorted)))
        | $unit + {calls: $calls, sorted: $cap.sorted}
      end
  ]
# A pick is named on the line of the command whose words make it. A unit that
# holds other reported units is judged again on the commands left once theirs
# are set aside, and is dropped when those make no pick: it was reported only
# for what it contains, which is already named on its own line.
| . as $found
| map(. as $outer
    | [$found[] | select(.offset >= $outer.offset and .end <= $outer.end and (.offset != $outer.offset or .end != $outer.end))] as $inner
    | select(($inner | length) == 0 or
        ([$outer.calls[] | . as $c | select(any($inner[]; $c.offset >= .offset and $c.offset < .end) | not) | .words[]]
         | join(" ") | picks($outer.sorted))))
| sort_by(.offset)[]
| "\($file):\(.line)"
