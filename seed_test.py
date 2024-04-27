import multiprocessing
import subprocess
import os

def run_command(i):
    print(f"running {i}")

    cmd = [os.path.expanduser("~/.cargo/bin/cargo"), *"miri test -- --nocapture".split()]
    p = subprocess.run(cmd,
        env={'MIRIFLAGS': f'-Zmiri-seed={i}'},
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True)

    if p.returncode != 0:
        print(f"ERROR {i}\n{p.stdout}")
    else:
        print(f"ok {i}")

if __name__ == "__main__":
    multiprocessing.freeze_support()

    num_cores = multiprocessing.cpu_count()
    with multiprocessing.Pool(processes=num_cores) as pool:
        pool.map(run_command, range(1000))


