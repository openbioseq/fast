# conda-forge packaging

This directory contains the recipe for submitting `fast` to
[conda-forge](https://conda-forge.org/).

## Submit to conda-forge

1. Fork https://github.com/conda-forge/staged-recipes.
2. Create a branch and copy `recipe/` to `recipes/fast/`.
3. Open a pull request against `conda-forge/staged-recipes`.

The v1 recipe (`recipe.yaml`) builds from the tagged source archive, runs
`cargo install` with the conda-forge Rust toolchain, and installs the
`fast` binary. `cargo-bundle-licenses` generates `THIRDPARTY.yml` so that
dependency licenses are included in the conda package.

## Local build

```bash
conda install -n base -c conda-forge rattler-build
rattler-build build -r conda/recipe/recipe.yaml
```

After the staged-recipes pull request is merged and the feedstock is
created, install with:

```bash
conda install -c conda-forge fast
```
