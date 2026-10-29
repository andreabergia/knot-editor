use std::collections::HashMap;
// <- keyword.import
//   ^ module
//                    ^ type

/// Documentation.
// <- comment.documentation

const LIMIT: usize = 42;
// <- keyword.modifier
//    ^ constant
//           ^ type.builtin
//                   ^ number

#[derive(Debug)]
//^ function.macro

pub struct Point<'a> {
//         ^ type
//                ^ attribute
    name: &'a str,
//  ^ variable.member
}

impl Point<'_> {
    pub fn new(name: &str) -> Self {
//      ^ keyword.function
//         ^ function
//             ^ variable.parameter
//                            ^ type
        let label = format!("{name}\n");
//      ^ keyword
//                  ^ function.macro
//                          ^ string
//                                 ^ string.escape
        if label.is_empty() {
//      ^ keyword.conditional
//                 ^ function.call
            return Self { name: "" };
//          ^ keyword.return
        }
        // A comment.
//      ^ comment
        Self { name }
    }
}

fn main() {
    let numbers = vec![1, 2];
    for n in numbers.iter().map(|n| n * 2) {
//  ^ keyword.repeat
        println!("{n}");
    }
    let _ = HashMap::<String, bool>::new();
//                            ^ type.builtin
//                                   ^ function.call
    let plain = "a\n";
//                 ^ string.escape
    let flag = true;
//             ^ boolean
}
