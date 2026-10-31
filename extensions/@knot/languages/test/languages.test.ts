import { testLanguagePackage, testLanguageSet } from "knot/testing/languages";
import { start } from "../src/extension.js";
import { LANGUAGES } from "../src/languages.js";

testLanguagePackage(start, LANGUAGES);
testLanguageSet(LANGUAGES);
