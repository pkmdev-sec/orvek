group "default" {
  targets = ["dev"]
}

target "dev" {
  context = "."
  dockerfile = "docker/development.dockerfile"
  tags = ["orvek-dev:local"]
}
