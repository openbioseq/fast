# Bioconda packaging

This directory contains the recipe for submitting `fast` to
[Bioconda](https://bioconda.github.io/).

## Submit to Bioconda

1. Fork https://github.com/bioconda/bioconda-recipes.
2. Create a branch and copy `recipe/` to `recipes/fast/`.
3. Open a pull request against `bioconda/bioconda-recipes`.

The recipe builds the Rust binary from the tagged source archive with
`cargo install` and uses `cargo-bundle-licenses` to package dependency
licenses.

## Local build

```bash
conda install -n base -c conda-forge -c bioconda conda-build
conda build bioconda/recipe -c conda-forge -c bioconda
```

After the pull request is merged and the package is uploaded, install with:

```bash
conda install -c bioconda fast
```
