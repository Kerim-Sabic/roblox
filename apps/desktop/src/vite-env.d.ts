/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_FORCE_MOCK?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
