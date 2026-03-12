import functional from "eslint-plugin-functional"
import oxlint from "eslint-plugin-oxlint"

export default [
  ...oxlint.buildFromOxlintConfigFile("./oxlint.config.ts"), // no overlap

  {
    languageOptions: {
      parserOptions: { project: "./tsconfig.json" },
    },
    plugins: { functional },
    rules: {
      // these are the only 💭 type-aware FP rules worth keeping
      "functional/immutable-data": "error",
      "functional/prefer-immutable-types": "error",
      "functional/type-declaration-immutability": "warn",
      "functional/no-mixed-types": "warn",
      "functional/no-return-void": "warn",
      "functional/no-throw-statements": "error", // type-aware version
    },
  },
]
