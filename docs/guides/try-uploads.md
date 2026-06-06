# Try uploads on this machine

Kapto uploads each episode to storage you own: S3 or S3-compatible storage,
Google Cloud Storage, Azure, or a folder. To see it work without a cloud
account, start RustFS, an S3-compatible server, on this machine with
Docker, and point Kapto's demo at it.

## Start the storage

From a clone of the repository:

```sh
docker compose run --rm rustfs-bucket
```

This starts RustFS at <http://localhost:9000>, reachable from this machine
only, waits until it answers and makes the bucket `kapto` in it; RustFS
carries on in the background. Its access key is `kapto` and its
secret key `kapto-storage`: they guard nothing but this test server, which
keeps its objects in the Docker volume `rustfs-data` until it is removed.

## Point Kapto at it

Save as `kapto.toml`:

```toml
[[upload.destination]]
name = "local"
url = "s3://kapto"
endpoint = "http://localhost:9000"
region = "us-east-1"
credentials = { env = "LOCAL" }
```

Kapto never keeps credentials in its configuration: `credentials` says to
read them from environment variables named after `LOCAL`. Set them, check
that Kapto can write, read back and remove a probe there, and start the
demo with this configuration:

```sh
export LOCAL_ACCESS_KEY_ID=kapto
export LOCAL_SECRET_ACCESS_KEY=kapto-storage
kapto --config kapto.toml upload check
kapto --config kapto.toml demo
```

`upload check` says `local: ok` once the storage answers; anything else
comes with its cause code and what to do. From a clone without Kapto
installed, run `cargo run --release --bin kapto --` in place of `kapto`.

## Watch an episode go up

Open <http://localhost:7700>, record an episode with **Record**, and stop
it. Once the worker has verified it, it goes up to `local`:

- **Uploads** shows each destination's uploaded, uploading and failed
  episodes, each upload going with how fast it goes and how long it has
  left, and a **Test** button that runs the same check as above.
- Each episode's details, in **Episodes**, show where each of its files
  went, with buttons to copy the episode's folder URI and each file's.

At the destination, an episode is its files, with `_SUCCESS` written last
listing each with its size and SHA-256: whoever reads the bucket treats an
episode as complete only once it is there.

## With Kapto in Docker

Kapto in a container reaches RustFS by its service name, so the endpoint is
`http://rustfs:9000` in place of `http://localhost:9000` in `kapto.toml`,
and Compose runs it on the same network:

```sh
docker compose run --rm -p 127.0.0.1:7700:7700 -p 127.0.0.1:9877:9877 \
  -v ./kapto.toml:/config/kapto.toml:ro \
  -e LOCAL_ACCESS_KEY_ID=kapto -e LOCAL_SECRET_ACCESS_KEY=kapto-storage \
  kapto demo
```

## Stop it

```sh
docker compose --profile storage down --volumes
```

stops RustFS and removes its volume, with every object uploaded to it.
Without `--volumes`, they are still there the next time it starts.

To upload to your own storage instead, see
[upload destinations](../concepts/configuration.md#upload-destinations).
