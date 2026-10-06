/** `npm run demo`: offline seed replay, compared with the Rust golden file. */
import { demo } from "./cli.js";

process.exit(demo({ out: (s) => process.stdout.write(s + "\n"), err: (s) => process.stderr.write(s + "\n") }));
