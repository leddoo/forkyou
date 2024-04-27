import multiprocessing
import subprocess
import os

def run_command(i):
    #print(f"running {i}")

    #cmd = [os.path.expanduser("~/.cargo/bin/cargo"), *"test --tests for_each -- --test-threads=1".split()]
    cmd = "./target/debug/deps/forkyou-ec4eb15be395a404 --test-threads=1 --nocapture".split()
    p = subprocess.run(cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True)

    if p.returncode != 0:
        print(f"ERROR {p.returncode}\n{p.stdout}")

if __name__ == "__main__":
    multiprocessing.freeze_support()

    num_cores = multiprocessing.cpu_count()
    with multiprocessing.Pool(processes=num_cores) as pool:
        for _ in range(1000):
            pool.map(run_command, range(1000))
            print("passed 1000")


