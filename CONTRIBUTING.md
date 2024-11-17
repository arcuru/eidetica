# Contributing

## Running Tests

Tests require standing up a local postgres service.
For simplicity, the only officially supported setup is using Docker and the scripts provided here.

Initialize:

```bash
$ task up
```

And to turn off the database:

```bash
$ task down
```

## CLAs

See the main [README.md](./README.md) for more details on the license, but as a reminder this is licensed under [FSL-1.1-MIT](https://fsl.software/).

Even with the non-standard license, this repo does not require a CLA to contribute, and any contributions will fall under the current license.

[Githubs own terms of service](https://docs.github.com/en/site-policy/github-terms/github-terms-of-service#6-contributions-under-repository-license) explicitly clarify that you are agreeing that any contributions to this repo will fall under the same license:

```quote
Whenever you add Content to a repository containing notice of a license, you license that Content under the same terms, and you agree that you have the right to license that Content under those terms. If you have a separate agreement to license that Content under different terms, such as a contributor license agreement, that agreement will supersede.

Isn't this just how it works already? Yep. This is widely accepted as the norm in the open-source community; it's commonly referred to by the shorthand "inbound=outbound". We're just making it explicit.
```
