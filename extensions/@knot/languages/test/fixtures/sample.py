#!/usr/bin/env python3
# <- keyword.directive
import sys
# <- keyword.import
from dataclasses import dataclass
# <- keyword.import


@dataclass
# <- attribute
#  ^ attribute
class Point:
# <- keyword.type
#     ^ type
    """A point."""
#   ^ string.documentation
    x: int = 0
#   ^ variable.member
#      ^ type.builtin
#            ^ number

    def scaled(self, factor: float) -> "Point":
#   ^ keyword.function
#       ^ function.method
#              ^ variable.builtin
#                    ^ variable.parameter
        return Point(self.x * factor)
#       ^ keyword.return
#              ^ constructor
#                         ^ variable.member


def main() -> None:
#             ^ constant.builtin
    for arg in sys.argv[1:]:
#   ^ keyword.repeat
#           ^ keyword.operator
        if arg == "-v" and not False:
#       ^ keyword.conditional
#                      ^ keyword.operator
#                              ^ boolean
            print(f"verbose {arg}\n")
#           ^ function.builtin
#                 ^ string
#                                ^ string.escape
    # Done.
#   ^ comment
