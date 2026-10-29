import { readFile } from "node:fs/promises";
// <- keyword.import
//                       ^ string

export interface Options {
//     ^ keyword.type
//               ^ type
  readonly path: string;
//^ keyword.modifier
//         ^ variable.member
//               ^ type.builtin
}

export async function load(options: Options): Promise<number> {
//     ^ keyword.coroutine
//           ^ keyword.function
//                    ^ function
//                         ^ variable.parameter
  const text = await readFile(options.path, "utf8");
//^ keyword
//             ^ keyword.coroutine
//                   ^ function.call
//                                    ^ variable.member
  if (text.length === 0) {
//^ keyword.conditional
//                ^ operator
//                    ^ number
    throw new Error(`empty ${options.path}`);
//  ^ keyword.exception
//            ^ constructor
//                  ^ string
//                           ^ variable
  }
  // Count lines.
//^ comment
  return text.split("\n").length;
//^ keyword.return
//            ^ function.method.call
//                   ^ string.escape
}

const enabled = true as boolean;
//              ^ boolean
//                   ^ keyword.operator
const pattern = /^a+$/g;
//              ^ punctuation.bracket
//               ^ string.regexp
