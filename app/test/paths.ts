import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url)); // app/dist/test
export const APP = resolve(here, "../..");
export const SEED = resolve(APP, "../data/seed");
export const FIXTURES = resolve(APP, "fixtures");
export const SNAPSHOTS = resolve(APP, "test/snapshots");
