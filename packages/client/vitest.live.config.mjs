// `pnpm test:live`: the real-tunnel suite only (needs network, cargo, TailPocket).
export default { test: { include: ["test/**/*.live.test.ts"], fileParallelism: false } };
