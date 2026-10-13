#!/bin/sh
# Synthetic protocol peer. No credentials, model calls, or SDK runtime.
case "$1" in
  --version) printf 'synthetic-claude-2\n'; exit 0 ;;
  --help) printf '%s\n' '--input-format --output-format --include-partial-messages --permission-prompt-tool --session-id --resume --append-system-prompt --setting-sources --strict-mcp-config'; exit 0 ;;
esac
previous=''
for argument do
  case "$previous" in --session-id|--resume) session="$argument" ;; esac
  previous="$argument"
  if [ -n "$FAKE_ARGS" ]; then printf '%s\n' "$argument" >> "$FAKE_ARGS"; fi
done
result() {
  printf '{"type":"result","session_id":"%s","subtype":"success","result":"%s","usage":{"input_tokens":10,"output_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":2},"total_cost_usd":0.01}\n' "$session" "$1"
}
while IFS= read -r line; do
  case "$line" in
    *'"subtype":"initialize"'*)
      id=$(printf '%s' "$line" | sed 's/.*"request_id":"\([^"]*\)".*/\1/')
      printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$id"
      ;;
    *'"type":"user"'*)
      if [ -n "$FAKE_INPUT" ]; then printf '%s\n' "$line" >> "$FAKE_INPUT"; fi
      case "$FAKE_MODE" in
        approval|question)
          if [ "$FAKE_MODE" = question ]; then
            printf '{"type":"control_request","request_id":"tool-1","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","tool_use_id":"use-1","input":{"questions":[{"question":"Which?","options":[{"label":"A","description":"first"},{"label":"B","description":"second"}],"multiSelect":false}]}}}\n'
          else
            printf '{"type":"control_request","request_id":"tool-1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"use-1","input":{"command":"printf original","token":"secret-original"}}}\n'
          fi
          ;;
        binding_error) rm -f "$FAKE_BINDING"; mkdir "$FAKE_BINDING"; result done ;;
        bad_json) printf 'not json\n' ;;
        oversized) awk 'BEGIN { for(i=0;i<1048577;i++) printf "x"; printf "\n" }' ;;
        truncated) printf '{"type":"result"}'; exit 0 ;;
        stderr) awk 'BEGIN { for(i=0;i<200000;i++) printf "error\n" }' >&2; result done ;;
        background)
          printf '{"type":"system","subtype":"task_started","task_type":"local_agent","task_id":"child"}\n'
          printf '{"type":"system","subtype":"session_state_changed","state":"running"}\n'
          result initial
          sleep 0.3
          printf '{"type":"system","subtype":"task_updated","task_id":"child","patch":{"status":"completed"}}\n'
          printf '{"type":"assistant","session_id":"%s","message":{"id":"continuation","content":[{"type":"text","text":"continued"}]}}\n' "$session"
          result continued
          printf '{"type":"system","subtype":"session_state_changed","state":"idle"}\n'
          ;;
        hanging) sleep 60 ;;
        *) result done ;;
      esac
      ;;
    *'"type":"control_response"'*)
      if [ -n "$FAKE_CONTROL" ]; then printf '%s\n' "$line" >> "$FAKE_CONTROL"; fi
      case "$line" in *'"behavior":"allow"'*) result allowed ;; *) result denied ;; esac
      ;;
    *'"subtype":"interrupt"'*) exit 0 ;;
  esac
done
