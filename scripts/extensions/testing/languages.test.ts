// Checks the languages every bundled language package registers, together.
import { LANGUAGES as markdown } from "../../../extensions/@knot/language-markdown/src/languages.js";
import { LANGUAGES as others } from "../../../extensions/@knot/language-others/src/languages.js";
import { LANGUAGES as python } from "../../../extensions/@knot/language-python/src/languages.js";
import { LANGUAGES as rust } from "../../../extensions/@knot/language-rust/src/languages.js";
import { LANGUAGES as toml } from "../../../extensions/@knot/language-toml/src/languages.js";
import { LANGUAGES as typescript } from "../../../extensions/@knot/language-typescript/src/languages.js";
import { testLanguageSet } from "./languages.js";

testLanguageSet([...markdown, ...others, ...python, ...rust, ...toml, ...typescript]);
