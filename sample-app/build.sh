#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "=== Building JavaPaaS Sample Application ==="

# 1. Locate javac
JAVAC_BIN=""
if [[ -n "${JAVA_HOME:-}" && -x "$JAVA_HOME/bin/javac" ]]; then
    JAVAC_BIN="$JAVA_HOME/bin/javac"
elif command -v javac >/dev/null 2>&1; then
    JAVAC_BIN="$(command -v javac)"
elif [[ -x "/opt/pycharm-2025.3.3/jbr/bin/javac" ]]; then
    JAVAC_BIN="/opt/pycharm-2025.3.3/jbr/bin/javac"
elif [[ -x "/usr/lib/jvm/default-java/bin/javac" ]]; then
    JAVAC_BIN="/usr/lib/jvm/default-java/bin/javac"
fi

if [[ -z "$JAVAC_BIN" ]]; then
    echo "ERROR: javac not found! Please set JAVA_HOME or install a JDK." >&2
    exit 1
fi

echo "Using javac: $JAVAC_BIN ($("$JAVAC_BIN" -version 2>&1))"

# 2. Clean and create target directories
rm -rf classes target
mkdir -p classes target

# 3. Compile Java sources
echo "Compiling Java sources..."
"$JAVAC_BIN" -d classes -sourcepath src src/com/javapaas/sample/SampleApp.java

# 4. Create JAR archive
OUTPUT_JAR="$SCRIPT_DIR/target/sample-app.jar"
echo "Packaging $OUTPUT_JAR..."

if command -v jar >/dev/null 2>&1; then
    jar --create --file "$OUTPUT_JAR" --main-class com.javapaas.sample.SampleApp -C classes .
elif [[ -x "$(dirname "$JAVAC_BIN")/jar" ]]; then
    "$(dirname "$JAVAC_BIN")/jar" --create --file "$OUTPUT_JAR" --main-class com.javapaas.sample.SampleApp -C classes .
elif command -v zip >/dev/null 2>&1; then
    mkdir -p classes/META-INF
    cat << 'EOF' > classes/META-INF/MANIFEST.MF
Manifest-Version: 1.0
Main-Class: com.javapaas.sample.SampleApp
Created-By: JavaPaaS-Build

EOF
    (cd classes && zip -q -r "$OUTPUT_JAR" META-INF com)
else
    python3 -c "
import os, zipfile
jar_path = '$OUTPUT_JAR'
classes_dir = '$SCRIPT_DIR/classes'
with zipfile.ZipFile(jar_path, 'w', zipfile.ZIP_DEFLATED) as zf:
    manifest = b'Manifest-Version: 1.0\r\nMain-Class: com.javapaas.sample.SampleApp\r\nCreated-By: JavaPaaS-Build\r\n\r\n'
    zf.writestr('META-INF/MANIFEST.MF', manifest)
    for root, _, files in os.walk(classes_dir):
        for f in files:
            full = os.path.join(root, f)
            rel = os.path.relpath(full, classes_dir)
            if not rel.startswith('META-INF'):
                zf.write(full, rel)
"
fi

if [[ -f "$OUTPUT_JAR" ]]; then
    echo "SUCCESS: Built JAR at $OUTPUT_JAR ($(ls -lh "$OUTPUT_JAR" | awk '{print $5}'))"
else
    echo "ERROR: Failed to create $OUTPUT_JAR" >&2
    exit 1
fi
