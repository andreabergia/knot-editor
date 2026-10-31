import type { ReactNode } from "react";

export function Badge({ label }: { label: string }): ReactNode {
  return <span className="badge">{label}</span>;
//        ^ tag.builtin
//             ^ tag.attribute
//                       ^ string
//                                ^ variable
//                                       ^ tag.delimiter
//                                        ^ tag.builtin
}

export const Empty = () => <Badge label="none" />;
//                          ^ tag
