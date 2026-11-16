#!/usr/bin/env bash
# The shell in every demo pane: bash with only bashrc, and no inputrc, whose
# mode markers would show in the prompt.
export INPUTRC=/dev/null
exec bash --noprofile --rcfile "$(dirname "$0")/bashrc" -i
