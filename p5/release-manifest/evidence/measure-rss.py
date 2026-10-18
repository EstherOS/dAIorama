import resource
import subprocess
import sys

# Run one child and report its peak resident set (KiB) plus exit code. One child per process,
# so RUSAGE_CHILDREN is exactly this child's peak.
completed = subprocess.run(sys.argv[1:], capture_output=True, text=True)
usage = resource.getrusage(resource.RUSAGE_CHILDREN)
print(f"rc={completed.returncode} max_rss_kib={usage.ru_maxrss}")
for line in (completed.stdout + completed.stderr).splitlines():
    print(line)
