# JavaPaaS Sample Application

A reference guest application specifically designed to test, validate, and benchmark the **JavaPaaS** runtime platform.

It has **zero external Maven/Gradle dependencies**, relying entirely on the standard Java SE runtime (`com.sun.net.httpserver.HttpServer`). It compiles in milliseconds using `javac` and packages into an executable JAR.

---

## Features & Verification Endpoints

| Endpoint | Method | Description | PaaS Verification Target |
| :--- | :--- | :--- | :--- |
| `/health` | `GET` | Returns HTTP 200 `{"status": "UP", ...}` and memory stats | Readiness and Liveness health probing in `Resurrector::probeHealth` |
| `/info` | `GET` | Introspects JVM version, threads, CPU cores, and `/proc/self/cgroup` | Cgroup hierarchy placement and JVM configuration |
| `/crash` | `POST` | Invokes `Runtime.getRuntime().halt(42)` to exit immediately | Watchdog crash detection (`SIGCHLD`), deduping, and auto-resurrection |
| `/stress-cpu` | `POST` | Spawns CPU-bound worker threads across available cores (`?duration=5`) | Cgroup CFS bandwidth enforcement (`cpu.max`) |
| `/stress-memory`| `POST` | Allocates byte arrays in heap memory (`?mb=500`) | Cgroup OOM limits (`memory.max`) and OOM-killer handling |

---

## Building

Run the self-contained build script:

```bash
./sample-app/build.sh
```

This compiles `SampleApp.java` and generates an executable JAR at `sample-app/target/sample-app.jar`.

---

## Running Standalone

You can run the application directly with any Java 17+ or 21+ runtime:

```bash
java -jar sample-app/target/sample-app.jar --port=8085 --name=my-tenant
```

Optional CLI flags and Environment variables:
- `--port=<port>` or `PORT=<port>` (Default: `8085`)
- `--name=<name>` (Default: `javapaas-sample-tenant`)

---

## Automated End-to-End Testing with JavaPaaS

To test the entire JavaPaaS lifecycle (Building -> Registration -> Spawn & Readiness Probing -> Live Tier Resize -> Crash Induction -> Auto-Resurrection -> Teardown), run:

```bash
./scripts/test_sample_e2e.sh
```
