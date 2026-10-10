# Advisory for local-spark-mcp: carry Fabric's Python packages in the runtime profiles (2026-10-09)

*From the Cobalt SQL Works side. Cobalt now installs a roster of Fabric Runtime 2.0's Python
packages into its LakeSail environment, so notebooks written for Fabric import what they expect.
The JVM engine (local-spark-mcp) would benefit from the same, and the profile is the natural
place for it. This note says where the list comes from, how we filter and install it, what we
learned, and what we would ask of `profiles.py`.*

## 1. The gap

A Fabric notebook assumes the runtime's environment: pandas, numpy, pyarrow, scikit-learn,
matplotlib, plotly, the azure-* clients, sqlalchemy, pyodbc, xgboost, nltk, and a long tail.
`profiles.py` pins the Spark side exactly (`pyspark==4.1.1`, `delta-spark==4.2.0`, the Python
and Java versions) and nothing else, so `import sklearn` in a cell that runs on Fabric fails
locally with `ModuleNotFoundError`. Users then add packages one by one through the host's
library settings, which drifts from Fabric's versions and has to be repeated per machine.

## 2. The source of truth

Microsoft publishes the full environment of every Fabric runtime in
<https://github.com/microsoft/synapse-spark-runtime>:

- `Fabric/Runtime 2.0 (Spark 4.1)/Fabric-Python313-CPU.yml` — a conda environment file: ~400
  entries, conda packages as `name=version=build` and a `pip:` section with `name==version`.
- `Fabric/Runtime 1.3 (Spark 3.5)/…` — the same for 1.3 (Python 3.11).
- `Components.json` and the `Official-…` / `Candidate-…` release notes alongside.

The file is the authoritative list, at Fabric's exact versions, and it moves with Fabric's
release channels, so a profile that cites the file's commit is reproducible.

## 3. How Cobalt turned it into an installable roster

The full file is not installable as-is: most entries are conda build strings, many are
Linux-only or GPU libraries, several are Fabric-only wheels that are not on PyPI. We curated
rather than mirrored:

1. Take every entry whose name is a Python package (ignore `_openmp_mutex`, `alsa-lib`, fonts,
   `libxxx` and other conda-only system packages).
2. Drop what the local engine replaces or cannot have: `pyspark` (the profile's own pin, or
   the Spark Connect client on LakeSail), `notebookutils`, `synapseml*`, `semantic-link-sempy`
   (Fabric-only wheels), `torch`/CUDA packages and anything with no wheel for Windows or macOS.
3. Keep the packages notebooks import, at Fabric's versions. Cobalt's 2.0 roster today
   (34 packages): pandas 2.3.3, numpy 2.4.1, pyarrow 24.0.0, scipy 1.18.0, scikit-learn 1.6.1,
   statsmodels 0.14.6, xgboost 2.1.4, lightgbm 4.6.0, matplotlib 3.10.9, seaborn 0.13.2,
   plotly 6.7.0, pillow 12.3.0, ipywidgets 8.1.7, requests 2.34.2, httpx 0.28.1, pyyaml 6.0.3,
   tqdm 4.68.3, jinja2 3.1.6, pydantic 2.13.4, openpyxl 3.1.5, xlrd 2.0.2, sqlalchemy 2.0.51,
   pyodbc 5.3.0, pandasql 0.7.3, azure-identity 1.25.3, azure-storage-blob 12.30.0,
   azure-storage-file-datalake 12.25.0, azure-keyvault-secrets 4.11.0, msal 1.37.0,
   msal-extensions 1.3.1, fsspec 2026.6.0, adlfs 2025.8.0, nltk 3.10.0, mlflow-skinny 2.22.0.
4. Record the source URL and the exclusions next to the list (Cobalt keeps them in its runtime
   manifest under `sail.fabric_packages["fabric-2.0"]` with `source` and `note` fields).

## 4. How the install behaves

- Opt-in: a profile choice (*None* / *fabric-2.0*), never installed unasked; it is a few
  hundred MB.
- One `uv pip install` of the whole roster first (one resolution, consistent versions). If that
  fails, install package by package and keep going: the ones that do not resolve on this
  machine are skipped and **named** in the status, never fatal. On Windows with Python 3.11 the
  only casualty was `scipy==1.18.0` (no wheel for that Python); a compatible scipy came in as a
  dependency, and the status says so.
- Status afterwards: "Fabric packages (fabric-2.0): 34 of 34 installed · not installable here:
  scipy==1.18.0", computed from the environment's dist-info, so it is honest after any
  later `pip` activity.
- The roster is a pin set, so bumping it is editing one list when Fabric moves (the yml file's
  commit is the reference).

## 5. The ask for local-spark-mcp

1. Give each profile in `profiles.py` an optional `python_packages` roster (name, version,
   source URL, exclusions), curated from the matching `Fabric-Python3xx-CPU.yml`, for both
   `fabric-1.3` and `fabric-2.0`.
2. Install it as a separate, opt-in step (`local-spark-mcp[fabric-2.0,packages]`, or a
   `--with-fabric-packages` flag on the environment install), with the batch-then-per-package
   behaviour and a machine-readable result (installed / skipped with reasons) the host can show.
3. Expose it in the manifest you already publish for hosts (version, Python, Java, Spark, Delta),
   so Cobalt can read the roster from the package instead of carrying its own copy, and the two
   engines offer the same packages at the same versions.
4. Keep `pyspark`, `delta-spark`, `notebookutils`, `synapseml*` and `semantic-link-sempy` out of
   the roster: the first two are the profile's own pins, the rest are Fabric-only.

Cobalt will switch its LakeSail roster to the package's list the moment it exists, so one
curated list serves both engines.
