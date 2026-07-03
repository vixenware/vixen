import { defineConfig } from "vite";

export default defineConfig({
  server: {
    fs: {
      // The vox TS runtime is link:'d from the sibling facet checkout.
      allow: ["../..", "../../../../oss/facet-cc"],
    },
  },
  build: {
    target: "esnext",
  },
});
