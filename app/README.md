# legion-app

The legion dashboard frontend. Vanilla TypeScript web components, no
framework. Compiled and **embedded into the legion binary**; rafters controls
(vendored via `pnpx rafters add`) replace the real UI from here.

Right now it is hello world -- one component proving the build pipeline.

## Build pipeline

```
src/*.ts  --(vite build)-->  app/dist/  --(rust-embed)-->  legion binary  -->  axum serves it
```

Node is build-time only. The runtime is a single Node-free binary.

## Dev

```
pnpm install
pnpm dev            # vite on :4000, proxies /api + /sse to legion on :3131
```

## rafters

```
pnpx rafters@latest init      # regenerates .rafters/ config + stylesheet
pnpx rafters add <control>    # vendors a control's .ts source locally
```
