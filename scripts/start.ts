import { resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
async function run(command: string[]) {
  const child = Bun.spawn(command, { cwd: root, stdin: "inherit", stdout: "inherit", stderr: "inherit" });
  const code = await child.exited;
  if (code !== 0) process.exit(code);
}

await run(["bun", "run", "build"]);
const server = Bun.spawn([resolve(root, "target/release/genetic-cars"), ...process.argv.slice(2)], {
  cwd: root, stdin: "inherit", stdout: "inherit", stderr: "inherit",
});
for (const signal of ["SIGINT", "SIGTERM"] as const) process.on(signal, () => server.kill(signal));
process.exit(await server.exited);
