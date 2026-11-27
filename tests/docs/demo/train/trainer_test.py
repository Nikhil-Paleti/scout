from train.trainer import Trainer, launch


def test_launch():
    assert launch(fake_config()) is not None
