document.addEventListener('DOMContentLoaded', () => {
    const images = document.querySelectorAll('.expandable-image');

    images.forEach(img => {
        img.addEventListener('click', () => {
            img.classList.toggle('expanded');
        });
    });
});
