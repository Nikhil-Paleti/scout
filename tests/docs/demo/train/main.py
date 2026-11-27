from train.trainer import launch


def main():
    tid, _, _ = parse_args()
    launch(load_config(tid))
