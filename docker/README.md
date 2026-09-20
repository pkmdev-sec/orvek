# Optional containers

Orvek runs natively. Containers are optional.

- [Development container](dev/README.md) runs tools against a mounted workspace and separates
  model credentials through `iron-proxy`.
- `release.dockerfile` packages a verified Linux binary at `/orvek`. It adds no shell or runtime.

No Orvek container image is published yet. The development Dockerfile currently expects
`ghcr.io/pkmdev-sec/orvek:latest`, so its build needs that release image or a configured replacement
binary stage. Use the native source installation until that prerequisite is available.

`docker-bake.hcl` defines the local `dev` target and release packaging target. A future release
workflow verifies the binary archive and compares the image binary before publishing GHCR tags.
Downstream runtime images must provide their own shell, CA certificates, and tools.
