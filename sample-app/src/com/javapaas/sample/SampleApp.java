package com.javapaas.sample;

import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpHandler;
import com.sun.net.httpserver.HttpServer;

import java.io.IOException;
import java.io.OutputStream;
import java.lang.management.ManagementFactory;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.time.Instant;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.Executors;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * SampleApp - A lightweight reference application for testing the JavaPaaS platform.
 *
 * Implements readiness probing, runtime introspection, CPU/memory stress testing,
 * and intentional crash triggering to verify the PaaS watchdog and auto-resurrector.
 */
public class SampleApp {

    private static final long START_TIME_MS = System.currentTimeMillis();
    private static final List<byte[]> MEMORY_LEAK_STORE = new ArrayList<>();

    public static void main(String[] args) throws Exception {
        int port = 8085;
        String appName = "javapaas-sample-tenant";

        for (String arg : args) {
            if (arg.startsWith("--port=")) {
                port = Integer.parseInt(arg.substring("--port=".length()).trim());
            } else if (arg.startsWith("--name=")) {
                appName = arg.substring("--name=".length()).trim();
            }
        }

        String envPort = System.getenv("PORT");
        if (envPort != null && !envPort.isEmpty()) {
            try {
                port = Integer.parseInt(envPort.trim());
            } catch (NumberFormatException ignored) {}
        }

        long pid = ProcessHandle.current().pid();
        System.out.printf("[JavaPaaS Sample App] Starting '%s' (PID: %d) on port %d...%n", appName, pid, port);

        HttpServer server = HttpServer.create(new InetSocketAddress("0.0.0.0", port), 0);

        server.createContext("/", new HealthHandler(appName));
        server.createContext("/health", new HealthHandler(appName));
        server.createContext("/actuator/health", new HealthHandler(appName));
        server.createContext("/info", new InfoHandler(appName));
        server.createContext("/crash", new CrashHandler());
        server.createContext("/stress-cpu", new CpuStressHandler());
        server.createContext("/stress-memory", new MemoryStressHandler());

        server.setExecutor(Executors.newVirtualThreadPerTaskExecutor());
        server.start();

        System.out.printf("[JavaPaaS Sample App] Ready and listening on http://0.0.0.0:%d%n", port);
        System.out.println("[JavaPaaS Sample App] Endpoints:");
        System.out.println("  - GET  /health          : Readiness and liveness probe");
        System.out.println("  - GET  /info            : System, JVM, memory, and cgroup metrics");
        System.out.println("  - POST /crash           : Trigger JVM crash to test PaaS resurrection");
        System.out.println("  - POST /stress-cpu      : Simulate CPU load to test CFS quota");
        System.out.println("  - POST /stress-memory   : Allocate heap to test cgroup memory.max");
    }

    private static void sendJsonResponse(HttpExchange exchange, int statusCode, String json) throws IOException {
        byte[] bytes = json.getBytes(StandardCharsets.UTF_8);
        exchange.getResponseHeaders().set("Content-Type", "application/json");
        exchange.sendResponseHeaders(statusCode, bytes.length);
        try (OutputStream os = exchange.getResponseBody()) {
            os.write(bytes);
        }
    }

    /**
     * Health check handler used for PaaS readiness probing.
     */
    static class HealthHandler implements HttpHandler {
        private final String appName;

        public HealthHandler(String appName) {
            this.appName = appName;
        }

        @Override
        public void handle(HttpExchange exchange) throws IOException {
            long uptimeSeconds = (System.currentTimeMillis() - START_TIME_MS) / 1000;
            Runtime runtime = Runtime.getRuntime();
            long totalMem = runtime.totalMemory() / (1024 * 1024);
            long freeMem = runtime.freeMemory() / (1024 * 1024);
            long maxMem = runtime.maxMemory() / (1024 * 1024);
            long pid = ProcessHandle.current().pid();

            String response = String.format(
                "{\n" +
                "  \"status\": \"UP\",\n" +
                "  \"application\": \"%s\",\n" +
                "  \"pid\": %d,\n" +
                "  \"uptime_seconds\": %d,\n" +
                "  \"memory\": {\n" +
                "    \"total_mb\": %d,\n" +
                "    \"used_mb\": %d,\n" +
                "    \"max_mb\": %d\n" +
                "  },\n" +
                "  \"timestamp\": \"%s\"\n" +
                "}",
                appName, pid, uptimeSeconds, totalMem, (totalMem - freeMem), maxMem, Instant.now()
            );

            sendJsonResponse(exchange, 200, response);
        }
    }

    /**
     * Detailed introspection info handler.
     */
    static class InfoHandler implements HttpHandler {
        private final String appName;

        public InfoHandler(String appName) {
            this.appName = appName;
        }

        @Override
        public void handle(HttpExchange exchange) throws IOException {
            long pid = ProcessHandle.current().pid();
            String javaVersion = System.getProperty("java.version");
            String javaVendor = System.getProperty("java.vendor");
            String osName = System.getProperty("os.name");
            String osArch = System.getProperty("os.arch");
            int availableProcessors = Runtime.getRuntime().availableProcessors();
            int threadCount = ManagementFactory.getThreadMXBean().getThreadCount();

            String cgroupInfo = "unknown";
            try {
                cgroupInfo = Files.readString(Paths.get("/proc/self/cgroup")).trim().replace("\n", "; ");
            } catch (Exception ignored) {}

            String response = String.format(
                "{\n" +
                "  \"application\": \"%s\",\n" +
                "  \"pid\": %d,\n" +
                "  \"java_version\": \"%s\",\n" +
                "  \"java_vendor\": \"%s\",\n" +
                "  \"os\": \"%s (%s)\",\n" +
                "  \"available_processors\": %d,\n" +
                "  \"active_threads\": %d,\n" +
                "  \"cgroup\": \"%s\"\n" +
                "}",
                appName, pid, javaVersion, javaVendor, osName, osArch,
                availableProcessors, threadCount, cgroupInfo
            );

            sendJsonResponse(exchange, 200, response);
        }
    }

    /**
     * Crash handler that triggers immediate JVM termination to verify
     * the JavaPaaS Watchdog and Controller resurrection pipeline.
     */
    static class CrashHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            System.err.println("[JavaPaaS Sample App] *** FORCED CRASH REQUEST RECEIVED ***");
            String response = "{\"status\": \"crashing\", \"message\": \"JVM terminating immediately...\"}";
            sendJsonResponse(exchange, 200, response);

            // Execute exit asynchronously to allow HTTP response to flush
            new Thread(() -> {
                try {
                    Thread.sleep(200);
                } catch (InterruptedException ignored) {}
                System.err.println("[JavaPaaS Sample App] Halting process with exit code 42");
                Runtime.getRuntime().halt(42);
            }).start();
        }
    }

    /**
     * CPU stress handler to exercise cgroup cpu.max throttling.
     */
    static class CpuStressHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            int durationSeconds = 3;
            String query = exchange.getRequestURI().getQuery();
            if (query != null && query.contains("duration=")) {
                for (String param : query.split("&")) {
                    if (param.startsWith("duration=")) {
                        try {
                            durationSeconds = Math.min(30, Integer.parseInt(param.substring("duration=".length())));
                        } catch (NumberFormatException ignored) {}
                    }
                }
            }

            final int duration = durationSeconds;
            final AtomicBoolean running = new AtomicBoolean(true);
            int cores = Runtime.getRuntime().availableProcessors();

            for (int i = 0; i < cores; i++) {
                Thread.startVirtualThread(() -> {
                    long counter = 0;
                    while (running.get()) {
                        counter = (counter * 31 + 17) ^ (counter >> 3);
                    }
                });
            }

            new Thread(() -> {
                try {
                    Thread.sleep(duration * 1000L);
                } catch (InterruptedException ignored) {}
                running.set(false);
            }).start();

            String response = String.format(
                "{\"status\": \"stressing_cpu\", \"duration_seconds\": %d, \"threads\": %d}",
                duration, cores
            );
            sendJsonResponse(exchange, 200, response);
        }
    }

    /**
     * Memory stress handler to exercise cgroup memory.max limits.
     */
    static class MemoryStressHandler implements HttpHandler {
        @Override
        public void handle(HttpExchange exchange) throws IOException {
            int mb = 100;
            String query = exchange.getRequestURI().getQuery();
            if (query != null && query.contains("mb=")) {
                for (String param : query.split("&")) {
                    if (param.startsWith("mb=")) {
                        try {
                            mb = Math.min(2048, Integer.parseInt(param.substring("mb=".length())));
                        } catch (NumberFormatException ignored) {}
                    }
                }
            }

            try {
                // Allocate requested chunks of 10 MB
                for (int i = 0; i < mb / 10; i++) {
                    byte[] chunk = new byte[10 * 1024 * 1024];
                    // Touch bytes to ensure physical memory allocation (cgroup accounting)
                    for (int j = 0; j < chunk.length; j += 4096) {
                        chunk[j] = 1;
                    }
                    MEMORY_LEAK_STORE.add(chunk);
                }

                long totalAllocatedMb = (long) MEMORY_LEAK_STORE.size() * 10;
                String response = String.format(
                    "{\"status\": \"allocated\", \"requested_mb\": %d, \"total_retained_mb\": %d}",
                    mb, totalAllocatedMb
                );
                sendJsonResponse(exchange, 200, response);
            } catch (OutOfMemoryError oom) {
                String response = String.format(
                    "{\"status\": \"oom\", \"error\": \"%s\"}",
                    oom.getMessage()
                );
                sendJsonResponse(exchange, 500, response);
            }
        }
    }
}
