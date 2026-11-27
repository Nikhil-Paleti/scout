import torch


class Trainer:
    """Runs the training loop; see launch()."""

    def __init__(self, model, lr=1e-3):
        self.model = model
        self.lr = lr  # TODO: warmup schedule

    def step(self, batch):
        loss = self.model(batch)
        return loss


def launch(config):
    trainer = Trainer(config.model)
    return trainer.step(config.batch)
