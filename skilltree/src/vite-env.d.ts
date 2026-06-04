/// <reference types="vite/client" />

declare module "*.yaml?raw" {
  const src: string;
  export default src;
}
