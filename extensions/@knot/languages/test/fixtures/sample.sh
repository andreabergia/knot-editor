#!/bin/sh
# <- keyword.directive
set -eu
# <- function.builtin
#   ^ variable.parameter

name="${1:-world}"
# <- variable
#      ^ punctuation.special
for file in *.txt; do
# <- keyword.repeat
#   ^ variable
#                  ^ keyword.repeat
  if [ -f "$file" ]; then
# ^ keyword.conditional
#      ^ operator
#           ^ variable
    echo "found $file" | grep -q found
#   ^ function.builtin
#        ^ string
#                      ^ operator
#                        ^ function.call
  fi
done
greet() {
# <- function
  printf '%s\n' "$name" > /dev/null
# ^ function.builtin
#        ^ string
#                       ^ operator
}
# A comment.
# <- comment
