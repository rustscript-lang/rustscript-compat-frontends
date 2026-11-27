import { factorial as factorialFromFile } from "../fixtures/typescript/factorial.ts";
const alias = factorialFromFile;
const result = alias(6);
result;
