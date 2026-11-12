// `*.live.test.ts` need a real tunnel and run only through `test:live`.
export default { test: { include: ["test/**/*.test.ts"], exclude: ["**/node_modules/**", "test/**/*.live.test.ts"] } };
